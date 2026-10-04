// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! What a view may say: closed enums of stable keys, never English text,
//! so a translation is a table and a new label is a compile error in
//! every exhaustive match below (agreed items 5, 6).

use interweave_human_client_api::{
    Connectivity, OutboundStatus, SendError, SendProblem, SessionProblem,
};

/// One list makes the enum, [`LabelKey::ALL`] and the keys, so a label
/// cannot exist outside the list P6's test walks (review F4): there is no
/// second list to forget.
macro_rules! labels {
    ($($(#[$doc:meta])* $variant:ident => $key:literal,)+) => {
        /// Every status label a view shows: closed, and P6's test walks
        /// every one of them.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum LabelKey {
            $($(#[$doc])* $variant,)+
        }

        impl LabelKey {
            /// Every label, from the same list as the enum.
            pub const ALL: &'static [Self] = &[$(Self::$variant,)+];

            /// The label's stable key, which a translation table maps to
            /// text.
            #[must_use]
            pub const fn key(self) -> &'static str {
                match self {
                    $(Self::$variant => $key,)+
                }
            }
        }
    };
}

labels! {
    /// Outbound, being sent.
    Sending => "status.sending",
    /// Outbound, the last attempt may have been received: "not
    /// confirmed", never "failed".
    NotConfirmed => "status.not_confirmed",
    /// Outbound, waiting for the person, nothing went out that may have
    /// reached the remote.
    NeedsAttention => "status.needs_attention",
    /// Outbound, waiting for the person, and an earlier attempt may
    /// already have been received.
    NeedsAttentionMayHaveBeenReceived => "status.needs_attention.may_have_been_received",
    /// Outbound, the remote transport's queue accepted it (`AcceptedV2`):
    /// not read, not seen, not processed.
    AcceptedByRemoteTransport => "status.accepted_by_remote_transport",
    /// Outbound, published by the local transport: not delivered to any
    /// recipient in particular.
    PublishedLocally => "status.published_locally",
    /// Outbound, cancelled; nothing that went out may have been received.
    Cancelled => "status.cancelled",
    /// Outbound, cancelled after an attempt that may have been received.
    CancelledMayHaveBeenReceived => "status.cancelled.may_have_been_received",
    /// Inbound, not yet read here.
    Unread => "status.unread",
    /// Inbound, read here and not kept: gone after a restart.
    ReadNotKept => "status.read_not_kept",
    /// Inbound, read here and kept.
    Kept => "status.kept",
}

impl LabelKey {
    /// Whether the label describes an outbound message's delivery: the
    /// labels P6 holds to "never read, seen or processed".
    #[must_use]
    pub const fn is_delivery(self) -> bool {
        !matches!(self, Self::Unread | Self::ReadNotKept | Self::Kept)
    }
}

/// Every other piece of person-facing text a view shows: closed, from one
/// list, so [`placeholder_en`] covers each and a test walks them all.
macro_rules! ui_texts {
    ($($(#[$doc:meta])* $variant:ident => $english:literal,)+) => {
        /// A piece of interface text that is not a status label or an
        /// error class: a control's name, a heading, a template.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum UiText {
            $($(#[$doc])* $variant,)+
        }

        impl UiText {
            /// Every one, from the same list as the enum.
            pub const ALL: &'static [Self] = &[$(Self::$variant,)+];
        }

        /// The placeholder English for `text`.
        const fn ui_text_en(text: UiText) -> &'static str {
            match text {
                $(UiText::$variant => $english,)+
            }
        }
    };
}

ui_texts! {
    /// The window's title.
    AppTitle => "InterWeave",
    /// A `PeerId` shortened for display: `{tail}` is its last eight
    /// characters, verbatim.
    ShortPeer => "…{tail}",
    /// A direct conversation's title: `{peer}` the short `PeerId`,
    /// `{route}` the route label the peer asserts.
    DirectTitle => "{peer} / {route}",
    /// The heading of the conversation list.
    Conversations => "Conversations",
    /// The heading of a conversation's messages.
    Messages => "Messages",
    /// The open conversation's full identifier, selectable to copy: a
    /// direct route's `PeerId` or a channel's id, in exact form.
    ConversationId => "Identifier, select to copy",
    /// Shown when no conversation is selected.
    NoConversation => "Select a conversation",
    /// What a direct conversation is, beside its title.
    DirectConversation => "Direct conversation",
    /// What a channel conversation is, beside its title.
    ChannelConversation => "Channel",
    /// A conversation's unread count. `{count}` is a number.
    UnreadCount => "{count} unread",
    /// A conversation row's description: `{kind}` is what the
    /// conversation is, `{unread}` its unread count's text.
    ConversationDescription => "{kind}, {unread}",
    /// A route label: `{route}` is the `EndpointId`, verbatim -- a
    /// routing label, never a name.
    Route => "route: {route}",
    /// The author of this client's own messages.
    You => "You",
    /// A message item as a screen reader reads it: `{author}` short,
    /// then `{status}`, then `{body}` (U5a).
    Item => "{author}, {status}: {body}",
    /// A link's control, after the message body: `{destination}` is the
    /// link's full destination, verbatim, so the person sees where it
    /// goes before activating it.
    OpenLink => "Open link: {destination}",
    /// An image the body references, in its place: never fetched.
    /// `{alt}` is the image's alt text, verbatim, possibly empty.
    ImageNotLoaded => "[Image not loaded: {alt}]",
    /// Show a message's source as received, in place of the drawn body.
    ShowSource => "Show source",
    /// Return from the source to the drawn body.
    ShowFormatted => "Show formatted",
    /// An unread message whose content is also kept (U2b).
    UnreadAlsoKept => "Unread, also kept",
    /// A reply whose target is shown in this conversation.
    ReplyPresent => "In reply to an earlier message",
    /// A reply whose target is not held here: neutral, never a doubt.
    ReplyUnavailable => "In reply to a message not shown here",
    /// The composer's text field.
    Composer => "Message",
    /// The composer's send control.
    Send => "Send",
    /// Why the last send made no row. `{reason}` is an error class's
    /// text.
    NotSent => "Not sent: {reason}",
    /// Retry a pending message now.
    Retry => "Retry",
    /// Cancel a pending message.
    Cancel => "Cancel",
    /// Keep a read message.
    Keep => "Keep",
    /// Remove Keep from a kept message.
    Unkeep => "Remove keep",
    /// No transport daemon serves this profile; the window connects once
    /// one starts.
    NoDaemon => "The transport daemon for this profile is not running. This window connects when it starts.",
    /// The session is re-opening on its own.
    Reconnecting => "Reconnecting",
    /// Storage cannot hold new messages.
    StorageDegraded => "Storage is full: new messages cannot be held",
    /// The session was refused. `{reason}` is an error class's text.
    Refused => "Could not open the session: {reason}",
    /// Leave a refused session.
    TryAgain => "Try again",
    /// Re-check storage now.
    RecheckStorage => "Check storage again",
}

/// The English shown until real copy exists.
///
/// DEVELOPMENT PLACEHOLDER COPY, UNREVIEWED. architect-cto's ruling on
/// relay message 01a0fe85-6b39-7d6e-8b2a-f0cc4280c7a8: these words ship in
/// Stage 14 as placeholders, held in this one module so Stage 15 can
/// replace it whole. Person-facing copy is never self-authored; final
/// copy waits for a language-culture remit. What they must already pass
/// is structural, and the tests below hold it: no delivery label reads
/// as read, seen, processed or delivered (P6), `Unknown` connectivity
/// never reads as offline, and every template is filled by placeholder,
/// never assembled by concatenation, so a translation may reorder it.
pub mod placeholder_en {
    use super::{Connectivity, ErrorClass, LabelKey, UiText};

    /// A status label.
    #[must_use]
    pub const fn label(key: LabelKey) -> &'static str {
        match key {
            LabelKey::Sending => "Sending",
            LabelKey::NotConfirmed => "Not confirmed",
            LabelKey::NeedsAttention => "Needs attention",
            LabelKey::NeedsAttentionMayHaveBeenReceived => {
                "Needs attention, may have been received"
            }
            LabelKey::AcceptedByRemoteTransport => "Accepted by remote transport",
            LabelKey::PublishedLocally => "Published locally",
            LabelKey::Cancelled => "Cancelled",
            LabelKey::CancelledMayHaveBeenReceived => "Cancelled, may have been received",
            LabelKey::Unread => "Unread",
            LabelKey::ReadNotKept => "Read this session",
            LabelKey::Kept => "Kept",
        }
    }

    /// An error class's message (`human-client-ui.md` §12).
    #[must_use]
    pub const fn error(class: ErrorClass) -> &'static str {
        match class {
            ErrorClass::PeerNotTrusted => "This peer is not trusted for this profile",
            ErrorClass::RouteUnavailable => "The selected route is currently unavailable",
            ErrorClass::NoNetworkPath => "No usable network path",
            ErrorClass::Busy => "The transport is temporarily busy",
            ErrorClass::TransportUnavailable => "The local transport is unavailable",
            ErrorClass::Incompatible => "No common protocol version",
            ErrorClass::TooLarge => "The message is too large to send",
            ErrorClass::NotConfigured => "This route or channel is not configured here",
            ErrorClass::InvalidMessage => "The message cannot be sent as written",
            ErrorClass::StorageUnavailable => "Local storage cannot hold it",
            ErrorClass::AlreadyPending => "This message is already waiting to be sent",
            ErrorClass::EndpointInUse => "This local endpoint is already in use by another client",
            ErrorClass::EndpointNotAvailable => "This endpoint is not available to this client",
            ErrorClass::Internal => "Something went wrong; details are in diagnostics",
        }
    }

    /// Connectivity as `human-client-ui.md` §7 normalizes it.
    #[must_use]
    pub const fn connectivity(state: Connectivity) -> &'static str {
        match state {
            Connectivity::OnlineDirect => "Online, direct reachable",
            Connectivity::OnlineRelay => "Online, relay available",
            Connectivity::OnlinePartial => "Online, outbound or partial reachability",
            Connectivity::Offline => "Offline, transport stopped",
            Connectivity::Unknown => "Connectivity not known yet",
        }
    }

    /// Any other interface text.
    #[must_use]
    pub const fn text(text: UiText) -> &'static str {
        super::ui_text_en(text)
    }
}

/// A `PeerId` as a view shows it in a list: its last eight characters,
/// verbatim, in the `ShortPeer` template. The full id is shown where a
/// person can read it whole (§11).
#[must_use]
pub fn short_peer(id: &str) -> String {
    let tail = &id[id.len().saturating_sub(8)..];
    fill(placeholder_en::text(UiText::ShortPeer), &[("tail", tail)])
}

/// `template` with each `{name}` replaced by its value, inserted verbatim
/// -- an id is never translated or reshaped (U4b). A name the template
/// does not hold is ignored; a placeholder left unfilled stays visible.
/// The test below holds that every template is filled by its own names;
/// that each CALL passes those names is held where the calls are, by
/// ui-slint's sweep of the rendered tree.
#[must_use]
pub fn fill(template: &str, values: &[(&str, &str)]) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let Some(close) = after.find('}') else {
            out.push_str(&rest[open..]);
            return out;
        };
        let name = &after[..close];
        if let Some((_, value)) = values.iter().find(|(n, _)| *n == name) {
            out.push_str(value);
        } else {
            out.push_str(&rest[open..=open + 1 + close]);
        }
        rest = &after[close + 1..];
    }
    out.push_str(rest);
    out
}

/// The label for an outbound status.
#[must_use]
pub const fn outbound_label(status: &OutboundStatus) -> LabelKey {
    match status {
        OutboundStatus::Sending { .. } => LabelKey::Sending,
        OutboundStatus::Unconfirmed { .. } => LabelKey::NotConfirmed,
        OutboundStatus::NeedsAttention {
            may_have_reached: false,
            ..
        } => LabelKey::NeedsAttention,
        OutboundStatus::NeedsAttention {
            may_have_reached: true,
            ..
        } => LabelKey::NeedsAttentionMayHaveBeenReceived,
        OutboundStatus::Accepted { .. } => LabelKey::AcceptedByRemoteTransport,
        OutboundStatus::Published => LabelKey::PublishedLocally,
        OutboundStatus::Cancelled {
            may_have_reached: false,
        } => LabelKey::Cancelled,
        OutboundStatus::Cancelled {
            may_have_reached: true,
        } => LabelKey::CancelledMayHaveBeenReceived,
    }
}

/// A user-actionable error class: one stable key per message
/// `human-client-ui.md` §12 asks for. The raw code stays in diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ErrorClass {
    /// The peer is not trusted for this profile.
    PeerNotTrusted,
    /// The selected route is currently unavailable.
    RouteUnavailable,
    /// No usable network path.
    NoNetworkPath,
    /// The remote or local transport is temporarily busy.
    Busy,
    /// The local transport is unavailable.
    TransportUnavailable,
    /// The two sides do not speak a common protocol version.
    Incompatible,
    /// The message is too large to send.
    TooLarge,
    /// This route or channel is not configured for this client.
    NotConfigured,
    /// The message is not one a receiver would accept.
    InvalidMessage,
    /// Local storage cannot hold it.
    StorageUnavailable,
    /// The same message is already pending.
    AlreadyPending,
    /// This local endpoint is already owned by another client or session.
    EndpointInUse,
    /// The endpoint is not available to this client.
    EndpointNotAvailable,
    /// Anything else; the raw code is in diagnostics.
    Internal,
}

/// The class of a send problem a row reports.
#[must_use]
pub const fn send_problem_class(problem: SendProblem) -> ErrorClass {
    match problem {
        SendProblem::PeerUntrusted => ErrorClass::PeerNotTrusted,
        SendProblem::RouteUnavailable => ErrorClass::RouteUnavailable,
        SendProblem::NoNetworkPath => ErrorClass::NoNetworkPath,
        SendProblem::Busy => ErrorClass::Busy,
        SendProblem::ServiceUnavailable => ErrorClass::TransportUnavailable,
        SendProblem::Incompatible => ErrorClass::Incompatible,
        SendProblem::TooLarge => ErrorClass::TooLarge,
        SendProblem::NotConfigured => ErrorClass::NotConfigured,
        SendProblem::Internal => ErrorClass::Internal,
    }
}

/// The class of a send the facade refused with no row.
#[must_use]
pub const fn send_error_class(error: &SendError) -> ErrorClass {
    match error {
        SendError::TooLarge => ErrorClass::TooLarge,
        SendError::InvalidEnvelope => ErrorClass::InvalidMessage,
        SendError::NotConfigured => ErrorClass::NotConfigured,
        SendError::StorageUnavailable => ErrorClass::StorageUnavailable,
        SendError::AlreadyPending => ErrorClass::AlreadyPending,
    }
}

/// The class of a session refusal.
#[must_use]
pub const fn session_problem_class(problem: SessionProblem) -> ErrorClass {
    match problem {
        SessionProblem::EndpointInUse => ErrorClass::EndpointInUse,
        SessionProblem::NotAvailableToThisClient => ErrorClass::EndpointNotAvailable,
        SessionProblem::Internal => ErrorClass::Internal,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_label_appears_once_in_all() {
        let mut keys: Vec<&str> = LabelKey::ALL.iter().map(|l| l.key()).collect();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), LabelKey::ALL.len(), "no label listed twice");
    }

    /// P6: no delivery label reads as read, seen or processed. Checked on
    /// the stable key AND the variant's name, since either is what a
    /// translator starts from.
    #[test]
    fn no_delivery_label_reads_as_read_seen_or_processed() {
        for label in LabelKey::ALL.iter().copied().filter(|l| l.is_delivery()) {
            let words = format!("{label:?} {}", label.key()).to_ascii_lowercase();
            for forbidden in ["read", "seen", "processed", "delivered"] {
                assert!(!words.contains(forbidden), "{label:?}: {forbidden}");
            }
        }
    }

    /// P6 on the words a person reads, not only the keys: no delivery
    /// label's English reads as read, seen, processed or delivered.
    #[test]
    fn no_delivery_text_reads_as_read_seen_or_processed() {
        for label in LabelKey::ALL.iter().copied().filter(|l| l.is_delivery()) {
            let words = placeholder_en::label(label).to_ascii_lowercase();
            for forbidden in ["read", "seen", "processed", "delivered"] {
                assert!(!words.contains(forbidden), "{label:?}: {words}");
            }
        }
    }

    #[test]
    fn unknown_connectivity_never_reads_as_offline() {
        let unknown = placeholder_en::connectivity(Connectivity::Unknown).to_ascii_lowercase();
        assert!(!unknown.contains("offline"), "{unknown}");
        assert!(
            placeholder_en::connectivity(Connectivity::Offline)
                .to_ascii_lowercase()
                .contains("offline"),
            "the control: Offline does say so"
        );
    }

    /// Every template is filled by name, and filling leaves no
    /// placeholder behind: a template is one string a translation may
    /// reorder, never pieces joined in code (U4b).
    #[test]
    fn every_template_is_filled_by_name() {
        let values = [
            ("count", "3"),
            ("route", "human"),
            ("author", "…abcd1234"),
            ("status", "Unread"),
            ("body", "hi"),
            ("reason", "busy"),
            ("kind", "Channel"),
            ("unread", "2 unread"),
            ("tail", "abcd1234"),
            ("peer", "…abcd1234"),
            ("destination", "https://example.org/a"),
            ("alt", "a cat"),
        ];
        for text in UiText::ALL {
            let filled = fill(placeholder_en::text(*text), &values);
            assert!(!filled.contains('{'), "{text:?}: {filled}");
        }
        assert_eq!(fill("route: {route}", &[("route", "a{b}")]), "route: a{b}");
        assert_eq!(
            fill("{missing}", &[]),
            "{missing}",
            "an unfilled one stays visible"
        );
    }

    #[test]
    fn a_short_peer_is_the_ids_tail_verbatim() {
        assert_eq!(short_peer("12D3KooWABCDEFGH12345678"), "…12345678");
        assert_eq!(short_peer("abc"), "…abc");
    }

    #[test]
    fn every_ui_text_appears_once_in_all() {
        let mut texts: Vec<&str> = UiText::ALL
            .iter()
            .map(|t| placeholder_en::text(*t))
            .collect();
        texts.sort_unstable();
        let n = texts.len();
        texts.dedup();
        assert_eq!(texts.len(), n, "no two interface texts read the same");
    }

    #[test]
    fn may_have_reached_gets_its_own_labels() {
        assert_ne!(
            outbound_label(&OutboundStatus::Cancelled {
                may_have_reached: true
            }),
            outbound_label(&OutboundStatus::Cancelled {
                may_have_reached: false
            })
        );
        assert_ne!(
            outbound_label(&OutboundStatus::NeedsAttention {
                problem: SendProblem::Busy,
                may_have_reached: true
            }),
            outbound_label(&OutboundStatus::NeedsAttention {
                problem: SendProblem::Busy,
                may_have_reached: false
            })
        );
    }

    #[test]
    fn endpoint_in_use_keeps_its_own_class() {
        assert_eq!(
            session_problem_class(SessionProblem::EndpointInUse),
            ErrorClass::EndpointInUse
        );
        assert_ne!(
            session_problem_class(SessionProblem::NotAvailableToThisClient),
            ErrorClass::EndpointInUse
        );
    }
}
