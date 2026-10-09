// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! What a view may say: closed enums of stable keys, never English text,
//! so a translation is a table and a new label is a compile error in
//! every exhaustive match below (agreed items 5, 6).

use interweave_human_client_api::{
    Connectivity, OutboundStatus, SendError, SendProblem, SessionProblem, TrustProblem,
};
use interweave_transport_api::PeerPath;

use crate::trust::EntryProblem;

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
    Route => "Route: {route}",
    /// The author of this client's own messages.
    You => "You",
    /// A message item as a screen reader reads it: `{author}` short,
    /// then `{status}`, then `{body}` (U5a).
    Item => "{author}, {status}: {body}",
    /// A link's control, after the message body: `{destination}` is the
    /// link's full destination as `visible_destination` shows it -- every
    /// character as sent, a hidden one as its code point -- so the person
    /// sees where it goes before activating it.
    OpenLink => "Open link: {destination}",
    /// An image the body references, in its place: never fetched.
    /// `{alt}` is the image's alt text, verbatim, possibly empty.
    ImageNotShown => "[Image not shown: {alt}]",
    /// Show a message's source as received, in place of the drawn body.
    ShowSource => "Show source",
    /// Return from the source to the drawn body.
    ShowFormatted => "Show formatted",
    /// The window's one announcement, read by a screen reader as it
    /// changes: a message arrived in the open conversation. `{author}`
    /// is the short `PeerId`; never the text, which is read by moving to
    /// the message.
    AnnounceArrival => "New message from {author}.",
    /// More than one arrived in the open conversation at once. `{count}`
    /// is a number.
    AnnounceArrivals => "New messages in this conversation: {count}.",
    /// One of this client's messages in the open conversation changed
    /// status. `{status}` is the status's text.
    AnnounceOwnStatus => "Your message: {status}.",
    /// More than one of them changed status at once. `{count}` is a
    /// number.
    AnnounceOwnStatuses => "Your messages changed status: {count}.",
    /// Messages arrived in one other conversation. `{conversation}` is
    /// its title.
    AnnounceElsewhere => "New messages in {conversation}.",
    /// Messages arrived in several other conversations. `{count}` is how
    /// many conversations.
    AnnounceElsewhereMany => "New messages in other conversations: {count}.",
    /// Two announcements made at once, `{first}` before `{rest}`.
    AnnounceBoth => "{first} {rest}",
    /// The open direct conversation's route indicator, once the runtime
    /// has said: the connection to the peer is its own (`human-client-ui.md`
    /// §7). Shown in place, never as a new event.
    PathDirect => "Connected directly",
    /// The same, through a relay's circuit.
    PathRelayed => "Connected through a relay",
    /// An unread message whose content is also kept (U2b).
    UnreadAlsoKept => "Unread, kept",
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
    Retry => "Send again",
    /// Cancel a pending message.
    Cancel => "Cancel",
    /// Keep a read message.
    Keep => "Keep",
    /// Remove Keep from a kept message.
    Unkeep => "Stop keeping",
    /// No transport daemon serves this profile; the window connects once
    /// one starts.
    NoDaemon => "The transport daemon for this profile is not running. This window will connect when the daemon starts.",
    /// The session is re-opening on its own.
    Reconnecting => "Reconnecting",
    /// Storage cannot hold new messages.
    StorageDegraded => "This device cannot store new messages now. You will not receive new messages until storage is available again.",
    /// The session was refused. `{reason}` is an error class's text.
    Refused => "Could not open the session: {reason}",
    /// Leave a refused session.
    TryAgain => "Try again",
    /// Re-check storage now.
    RecheckStorage => "Check storage again",
    /// The control that opens the trust settings, at the foot of the
    /// conversation list: a short button label.
    TrustSettings => "Trust settings",
    /// The control that leaves the trust settings for the conversations.
    BackToConversations => "Back to conversations",
    /// The trust settings' heading.
    TrustHeading => "Trust",
    /// Under the heading: what the list decides, and that a change made
    /// here lasts until it is changed again (ADR-0028 A 2026-10-07: the
    /// daemon keeps it in its state). The person's state: about to add or
    /// remove a peer; read once, so it may run to two sentences.
    TrustExplanation => "Only the peers listed here can exchange messages with this profile. A change made here stays until you change it again.",
    /// The label of this profile's own `PeerId`, shown in full and
    /// selectable to copy.
    OwnPeerId => "This profile's PeerId, select to copy",
    /// The heading of the trusted peers' list.
    TrustedPeers => "Trusted peers",
    /// Beside a listed peer that the profile's configuration lists: why it
    /// is there.
    TrustFromConfiguration => "From this profile's configuration",
    /// Beside a listed peer that was added in these settings.
    TrustAddedHere => "Added here",
    /// Shown in place of the list while it is read.
    TrustReading => "Reading the list of trusted peers",
    /// Shown in place of the list when it holds no peer.
    NoTrustedPeer => "No peer is trusted",
    /// The field a person types or pastes a `PeerId` into.
    PeerIdToTrust => "PeerId to trust",
    /// The control that proposes trusting the typed `PeerId`.
    TrustPeer => "Trust this peer",
    /// The control on a listed peer that proposes removing its trust.
    RemoveTrust => "Remove trust",
    /// The confirmation of an allow. `{peer}` is the exact `PeerId`,
    /// whole: the person must be able to check every character.
    ConfirmAllow => "Trust {peer} for this profile? It will be able to exchange messages with this profile until you remove trust from it.",
    /// The confirmation of a removal. `{peer}` is the exact `PeerId`,
    /// whole. The warning is human-client-ui.md section 8's: connections
    /// close at once.
    ConfirmRevoke => "Remove trust from {peer}? Its connections to this profile will close now, and it cannot exchange messages with this profile until it is trusted again.",
    /// Carry out the change on show.
    ConfirmChange => "Confirm",
    /// Drop the change on show.
    CancelChange => "Do not change",
    /// Said once the daemon allowed a peer. `{peer}` the exact `PeerId`.
    PeerTrusted => "{peer} is trusted.",
    /// Said once the daemon revoked a peer. `{peer}` the exact `PeerId`.
    PeerUntrusted => "{peer} is no longer trusted.",
    /// The typed text is not a `PeerId`.
    NotAPeerId => "That is not a PeerId. A PeerId starts with 12D3KooW or Qm. Check that you copied all of it.",
    /// The typed `PeerId` is this profile's own.
    OwnIdentity => "That is this profile's own PeerId. You do not need to trust it.",
    /// The typed `PeerId` is already listed.
    AlreadyTrusted => "That peer is already trusted.",
    /// Trust could not be read or changed: the daemon is not reachable.
    TrustUnavailable => "The transport daemon cannot be reached. Nothing was changed.",
    /// This client may not administer trust on this daemon.
    TrustNotPermitted => "This app is not allowed to change trust on this transport daemon. Nothing was changed.",
    /// The daemon refused the change: its own identity, or the list is
    /// full.
    TrustRefused => "The transport daemon refused this change. Nothing was changed.",
    /// The daemon and the app are not the same release, so they share no
    /// version trust needs: what helps is bringing both to one release,
    /// not trying again.
    TrustIncompatible => "The transport daemon is a different version from this app, so trust cannot be read or changed here. Nothing was changed.",
    /// Anything else went wrong, before anything was changed. No raw code
    /// is kept, so the text points nowhere for details.
    TrustFailed => "Trust could not be read or changed. Nothing was changed.",
    /// The daemon did not confirm a change, which may have been made: the
    /// list is read again. `{peer}` the exact `PeerId`, whole. Never says
    /// that nothing changed (TRANSPORT.md's outcome-unknown class).
    TrustUnconfirmed => "The transport daemon did not confirm the change for {peer}. The change may have been made. The list of trusted peers is being read again.",
    /// The list a made or possibly made change left to read again could
    /// not be read: it may not show that change. Never says that nothing
    /// changed. Opening the settings again reads it.
    TrustNotReadAgain => "The list of trusted peers could not be read again, so it may not show the latest change. Open the trust settings again to read it.",
}

/// The English the client shows.
///
/// Every value here was read by language-culture (relay message
/// 01a12001-199d-75d9-8e8d-cceb01ea9bbf): person-facing copy is never
/// self-authored, so a value added later ships as a placeholder its
/// author drafts and language-culture finalises. A drafted value carries
/// a line comment reading `PLACEHOLDER (en)` at the end of its line, or
/// on the line above it when the value wraps. The commit that takes
/// language-culture's final text removes it, so a search for that
/// comment lists every value still a draft. The module keeps its
/// Stage 14 name, from when these words shipped unreviewed under
/// architect-cto's ruling on relay message
/// 01a0fe85-6b39-7d6e-8b2a-f0cc4280c7a8. What every value must pass is
/// structural, and the tests below hold it: no delivery label reads as
/// read, seen, processed or delivered (P6), `Unknown` connectivity never
/// reads as offline, and every template is filled by placeholder, never
/// assembled by concatenation, so a translation may reorder it.
pub mod placeholder_en {
    use super::{Connectivity, EntryProblem, ErrorClass, LabelKey, PeerPath, TrustProblem, UiText};

    /// A status label.
    #[must_use]
    pub const fn label(key: LabelKey) -> &'static str {
        match key {
            LabelKey::Sending => "Sending",
            LabelKey::NotConfirmed => "Not confirmed, trying again",
            LabelKey::NeedsAttention => "Not sent, send again or cancel",
            LabelKey::NeedsAttentionMayHaveBeenReceived => {
                "May have reached the peer, send again or cancel"
            }
            LabelKey::AcceptedByRemoteTransport => "Accepted by remote transport",
            LabelKey::PublishedLocally => "Accepted by local transport",
            LabelKey::Cancelled => "Canceled",
            LabelKey::CancelledMayHaveBeenReceived => "Canceled, may have reached the peer",
            LabelKey::Unread => "Unread",
            LabelKey::ReadNotKept => "Read, not kept",
            LabelKey::Kept => "Kept",
        }
    }

    /// An error class's message (`human-client-ui.md` §12).
    #[must_use]
    pub const fn error(class: ErrorClass) -> &'static str {
        match class {
            ErrorClass::PeerNotTrusted => "This peer is not trusted for this profile.",
            ErrorClass::RouteUnavailable => "This route is not available now.",
            ErrorClass::NoNetworkPath => "This peer cannot be reached over the network now.",
            ErrorClass::Busy => "The transport is busy. Try again later.",
            ErrorClass::TransportUnavailable => "The transport daemon cannot be reached.",
            ErrorClass::Incompatible => "The two sides have no protocol version in common.",
            ErrorClass::TooLarge => "The message is too large to send.",
            ErrorClass::NotConfigured => "This route or channel is not configured here.",
            ErrorClass::InvalidMessage => "The message cannot be sent in this form.",
            ErrorClass::StorageUnavailable => "The message cannot be stored on this device.",
            ErrorClass::AlreadyPending => "This message is already waiting to be sent.",
            ErrorClass::EndpointInUse => "Another app or window is already using this endpoint.",
            ErrorClass::EndpointNotAvailable => "This endpoint is not available to this app.",
            ErrorClass::Internal => "Something went wrong.",
        }
    }

    /// A direct conversation's route indicator (`human-client-ui.md` §7).
    #[must_use]
    pub const fn path(path: PeerPath) -> &'static str {
        text(match path {
            PeerPath::Direct => UiText::PathDirect,
            PeerPath::Relayed => UiText::PathRelayed,
        })
    }

    /// Connectivity as `human-client-ui.md` §7 normalizes it.
    #[must_use]
    pub const fn connectivity(state: Connectivity) -> &'static str {
        match state {
            Connectivity::OnlineDirect => "Online, reachable directly",
            Connectivity::OnlineRelay => "Online, reachable through a relay",
            Connectivity::OnlinePartial => "Online, some peers might not reach you",
            Connectivity::Offline => "Offline, transport daemon not running",
            Connectivity::Unknown => "Network status not known yet",
        }
    }

    /// Any other interface text.
    #[must_use]
    pub const fn text(text: UiText) -> &'static str {
        super::ui_text_en(text)
    }

    /// Why trust was not read or changed.
    #[must_use]
    pub const fn trust_problem(problem: TrustProblem) -> &'static str {
        text(match problem {
            TrustProblem::Unavailable => UiText::TrustUnavailable,
            TrustProblem::NotPermitted => UiText::TrustNotPermitted,
            TrustProblem::Refused => UiText::TrustRefused,
            TrustProblem::Incompatible => UiText::TrustIncompatible,
            TrustProblem::Internal => UiText::TrustFailed,
        })
    }

    /// Why a typed `PeerId` was not proposed.
    #[must_use]
    pub const fn entry_problem(problem: EntryProblem) -> &'static str {
        text(match problem {
            EntryProblem::NotAPeerId => UiText::NotAPeerId,
            EntryProblem::OwnIdentity => UiText::OwnIdentity,
            EntryProblem::AlreadyTrusted => UiText::AlreadyTrusted,
        })
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

/// A link's destination as a view shows it on the link's control: every
/// control character, and every character Unicode makes default-ignorable
/// -- drawn as nothing, or as a change of direction or joining -- is shown
/// as its code point, `<U+202E>`, and every other character as it is. A
/// remote sender can otherwise put a right-to-left override in a
/// destination so that the label reads as one address while the opener
/// is given another; with it shown, no directional control reorders the
/// label. Right-to-left LETTERS are laid out by the ordinary bidirectional
/// rules, as in any text. The destination itself is not changed: what
/// opens is what was sent.
#[must_use]
pub fn visible_destination(destination: &str) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(destination.len());
    for c in destination.chars() {
        if is_hidden(c) {
            // Writing to a String cannot fail.
            let _ = write!(out, "<U+{:04X}>", u32::from(c));
        } else {
            out.push(c);
        }
    }
    out
}

/// Unicode's `Default_Ignorable_Code_Point` property
/// (`DerivedCoreProperties`, Unicode 16), with the interlinear annotation characters beside it:
/// what a renderer draws as nothing or applies rather than shows. A test
/// walks every range.
const HIDDEN: &[(char, char)] = &[
    ('\u{00AD}', '\u{00AD}'),
    ('\u{034F}', '\u{034F}'),
    ('\u{061C}', '\u{061C}'),
    ('\u{115F}', '\u{1160}'),
    ('\u{17B4}', '\u{17B5}'),
    ('\u{180B}', '\u{180F}'),
    ('\u{200B}', '\u{200F}'),
    ('\u{202A}', '\u{202E}'),
    ('\u{2060}', '\u{206F}'),
    ('\u{3164}', '\u{3164}'),
    ('\u{FE00}', '\u{FE0F}'),
    ('\u{FEFF}', '\u{FEFF}'),
    ('\u{FFA0}', '\u{FFA0}'),
    ('\u{FFF0}', '\u{FFFB}'),
    ('\u{1BCA0}', '\u{1BCA3}'),
    ('\u{1D173}', '\u{1D17A}'),
    ('\u{E0000}', '\u{E0FFF}'),
];

/// A control character, or one in [`HIDDEN`].
fn is_hidden(c: char) -> bool {
    c.is_control() || HIDDEN.iter().any(|&(low, high)| (low..=high).contains(&c))
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
            ("conversation", "…abcd1234 / human"),
            ("first", "New message from …abcd1234."),
            ("rest", "Your message: Sent."),
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
    fn a_destinations_hidden_characters_are_shown_and_the_rest_kept() {
        assert_eq!(
            visible_destination("https://evil.example/#\u{202E}elpmaxe.knab//:sptth"),
            "https://evil.example/#<U+202E>elpmaxe.knab//:sptth",
            "an override reads as its code point"
        );
        // Named apart from the table, at least one from each of its ranges,
        // so a range dropped from it or narrowed at an end fails here.
        let mut every: Vec<char> = vec![
            '\u{0007}',
            '\u{009F}',
            '\u{00AD}',
            '\u{034F}',
            '\u{061C}',
            '\u{115F}',
            '\u{1160}',
            '\u{17B4}',
            '\u{17B5}',
            '\u{180B}',
            '\u{180F}',
            '\u{200B}',
            '\u{200E}',
            '\u{200F}',
            '\u{202A}',
            '\u{202D}',
            '\u{2060}',
            '\u{2066}',
            '\u{2069}',
            '\u{206F}',
            '\u{3164}',
            '\u{FE00}',
            '\u{FE0F}',
            '\u{FEFF}',
            '\u{FFA0}',
            '\u{FFF0}',
            '\u{FFF9}',
            '\u{FFFB}',
            '\u{1BCA0}',
            '\u{1BCA3}',
            '\u{1D173}',
            '\u{1D17A}',
            '\u{E0000}',
            '\u{E0041}',
            '\u{E0100}',
            '\u{E0FFF}',
        ];
        for &(low, high) in HIDDEN {
            every.extend([low, high]);
            if let Some(middle) = char::from_u32(u32::midpoint(u32::from(low), u32::from(high))) {
                every.push(middle);
            }
        }
        for hidden in every {
            let shown = visible_destination(&format!("https://a.example/{hidden}x"));
            assert!(
                !shown.contains(hidden) && shown.contains("<U+"),
                "{:04X} is shown: {shown}",
                u32::from(hidden)
            );
        }
        let ordinary = "https://bücher.example/straße?q=日本&x=1#a-b_c~d";
        assert_eq!(
            visible_destination(ordinary),
            ordinary,
            "letters of any script stay as they are"
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
    fn every_trust_problem_reads_apart() {
        // An exhaustive match: a variant added to `TrustProblem` fails to
        // compile here until `listed` names it. The compiler does not see the
        // array below; add the variant there too, and raise the count.
        const fn listed(problem: TrustProblem) -> TrustProblem {
            match problem {
                TrustProblem::Unavailable
                | TrustProblem::NotPermitted
                | TrustProblem::Refused
                | TrustProblem::Incompatible
                | TrustProblem::Internal => problem,
            }
        }
        let mut texts: Vec<&str> = [
            TrustProblem::Unavailable,
            TrustProblem::NotPermitted,
            TrustProblem::Refused,
            TrustProblem::Incompatible,
            TrustProblem::Internal,
        ]
        .map(|p| placeholder_en::trust_problem(listed(p)))
        .to_vec();
        texts.sort_unstable();
        texts.dedup();
        assert_eq!(texts.len(), 5, "each problem says its own cause");
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
