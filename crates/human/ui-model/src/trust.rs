// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The trust settings' state (`human-client-ui.md` §8): the allowlist as
//! the daemon last said, the `PeerId` being typed, and a change waiting for
//! the person's confirmation. Pure: the root carries the intents out and
//! feeds back what the daemon answered.
//!
//! A change is never made from a proposal. The person proposes -- the
//! typed `PeerId`, or a listed peer's removal -- the view shows the exact
//! `PeerId` and the scope, and only [`TrustSettings::confirm`] yields the
//! intent that reaches the daemon. Nothing a message carries reaches
//! here: the inputs are the settings view's alone.

use interweave_human_client_api::{TrustList, TrustProblem};
use interweave_transport_api::TransportIdentity;

use crate::model::Intent;

/// One trust change: allow `peer`, or revoke it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TrustChange {
    /// The exact `PeerId`.
    pub peer: TransportIdentity,
    /// Allow it (`true`) or revoke it (`false`).
    pub allowed: bool,
}

/// Why a typed `PeerId` was not proposed: the person can correct it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EntryProblem {
    /// Not a `PeerId` in canonical form.
    NotAPeerId,
    /// This profile's own identity, never a peer to trust.
    OwnIdentity,
    /// Already on the allowlist.
    AlreadyTrusted,
}

/// What the last action came to, for the view to say once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrustOutcome {
    /// The daemon made the change and read back the allowlist.
    Changed(TrustChange),
    /// Reading or changing did nothing.
    Problem(TrustProblem),
    /// The typed `PeerId` was not proposed.
    Entry(EntryProblem),
}

/// What a person did in the trust settings, as a view hands it to the
/// root ([`crate::ViewEvent::Trust`]); the root applies it with
/// [`TrustSettings::input`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrustInput {
    /// The settings opened.
    Opened,
    /// The `PeerId` field now reads this.
    EntryChanged(String),
    /// Propose trusting the typed `PeerId`.
    ProposeAllow,
    /// Propose removing trust from a listed peer.
    ProposeRevoke(TransportIdentity),
    /// Carry out the change on show.
    Confirm,
    /// Drop the change on show.
    Cancel,
}

/// The trust settings, as a view renders them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TrustSettings {
    list: Option<TrustList>,
    reading: bool,
    entry: String,
    pending: Option<TrustChange>,
    in_flight: Option<TrustChange>,
    outcome: Option<TrustOutcome>,
    /// Bumped at every outcome, so the same outcome twice is still news.
    outcomes: u64,
}

impl TrustSettings {
    /// Apply what the person did: the intent it asks for, if any -- a
    /// read on opening, a change only on confirmation.
    pub fn input(&mut self, input: TrustInput) -> Option<Intent> {
        match input {
            TrustInput::Opened => self.opened(),
            TrustInput::EntryChanged(text) => {
                self.entry_changed(text);
                None
            }
            TrustInput::ProposeAllow => {
                self.propose_allow();
                None
            }
            TrustInput::ProposeRevoke(peer) => {
                self.propose_revoke(&peer);
                None
            }
            TrustInput::Confirm => self.confirm(),
            TrustInput::Cancel => {
                self.cancel();
                None
            }
        }
    }

    /// The view opened: read the allowlist, unless a read or a change is
    /// already on its way.
    pub fn opened(&mut self) -> Option<Intent> {
        if self.reading || self.in_flight.is_some() {
            return None;
        }
        self.reading = true;
        Some(Intent::ReadTrust)
    }

    /// The `PeerId` field now reads `text`.
    pub fn entry_changed(&mut self, text: String) {
        self.entry = text;
    }

    /// Propose trusting the typed `PeerId`: a confirmation to show, or why
    /// not. Whitespace around it is ignored, since a pasted `PeerId` often
    /// carries a newline; anything else must be the canonical form.
    pub fn propose_allow(&mut self) {
        let typed = self.entry.trim();
        let Ok(peer) = TransportIdentity::parse(typed) else {
            self.say(TrustOutcome::Entry(EntryProblem::NotAPeerId));
            return;
        };
        let list = self.list.as_ref();
        if list.is_some_and(|l| l.local_peer.as_ref() == Some(&peer)) {
            self.say(TrustOutcome::Entry(EntryProblem::OwnIdentity));
            return;
        }
        if list.is_some_and(|l| l.allowed.contains(&peer)) {
            self.say(TrustOutcome::Entry(EntryProblem::AlreadyTrusted));
            return;
        }
        self.pending = Some(TrustChange {
            peer,
            allowed: true,
        });
    }

    /// Propose removing trust from `peer`, which must be listed: a
    /// confirmation to show.
    pub fn propose_revoke(&mut self, peer: &TransportIdentity) {
        if self.list.as_ref().is_some_and(|l| l.allowed.contains(peer)) {
            self.pending = Some(TrustChange {
                peer: peer.clone(),
                allowed: false,
            });
        }
    }

    /// The person confirmed the change on show: the intent that makes
    /// it. The only way to one.
    pub fn confirm(&mut self) -> Option<Intent> {
        if self.in_flight.is_some() {
            return None;
        }
        let change = self.pending.take()?;
        self.in_flight = Some(change.clone());
        Some(Intent::SetTrust(change))
    }

    /// The person declined the change on show.
    pub fn cancel(&mut self) {
        self.pending = None;
    }

    /// The daemon's answer to a read.
    pub fn read(&mut self, answer: Result<TrustList, TrustProblem>) {
        self.reading = false;
        match answer {
            Ok(list) => self.list = Some(list),
            Err(problem) => self.say(TrustOutcome::Problem(problem)),
        }
    }

    /// The daemon's answer to `change`: the allowlist read back, or why
    /// nothing changed. The typed `PeerId` is cleared once it is trusted.
    pub fn set(&mut self, change: TrustChange, answer: Result<TrustList, TrustProblem>) {
        if self.in_flight.as_ref() == Some(&change) {
            self.in_flight = None;
        }
        match answer {
            Ok(list) => {
                if change.allowed && self.entry.trim() == change.peer.as_str() {
                    self.entry.clear();
                }
                self.list = Some(list);
                self.say(TrustOutcome::Changed(change));
            }
            Err(problem) => self.say(TrustOutcome::Problem(problem)),
        }
    }

    fn say(&mut self, outcome: TrustOutcome) {
        self.outcome = Some(outcome);
        self.outcomes = self.outcomes.wrapping_add(1);
    }

    /// The allowlist as last read, or `None` before the first read.
    #[must_use]
    pub const fn list(&self) -> Option<&TrustList> {
        self.list.as_ref()
    }

    /// Whether a read is on its way.
    #[must_use]
    pub const fn reading(&self) -> bool {
        self.reading
    }

    /// The `PeerId` field's text.
    #[must_use]
    pub fn entry(&self) -> &str {
        &self.entry
    }

    /// The change waiting for the person's confirmation.
    #[must_use]
    pub const fn pending(&self) -> Option<&TrustChange> {
        self.pending.as_ref()
    }

    /// The change sent and not answered yet.
    #[must_use]
    pub const fn in_flight(&self) -> Option<&TrustChange> {
        self.in_flight.as_ref()
    }

    /// The last outcome, and a count that changes with each, so a view
    /// announces one only when it is new.
    #[must_use]
    pub const fn outcome(&self) -> (Option<&TrustOutcome>, u64) {
        (self.outcome.as_ref(), self.outcomes)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]
    use super::*;
    use interweave_profile_identity::ProfileIdentity;

    fn peer() -> TransportIdentity {
        ProfileIdentity::generate()
            .transport_identity()
            .expect("a peer")
    }

    fn read(me: &TransportIdentity, allowed: &[TransportIdentity]) -> TrustSettings {
        let mut s = TrustSettings::default();
        assert_eq!(s.opened(), Some(Intent::ReadTrust));
        assert_eq!(s.opened(), None, "one read at a time");
        s.read(Ok(TrustList {
            local_peer: Some(me.clone()),
            allowed: allowed.to_vec(),
        }));
        s
    }

    #[test]
    fn a_typed_peer_is_trusted_only_through_its_confirmation() {
        let (me, them) = (peer(), peer());
        let mut s = read(&me, &[]);
        s.entry_changed(format!("  {}\n", them.as_str()));
        s.propose_allow();
        let change = TrustChange {
            peer: them.clone(),
            allowed: true,
        };
        assert_eq!(s.pending(), Some(&change), "shown for confirmation");
        assert_eq!(s.confirm(), Some(Intent::SetTrust(change.clone())));
        assert_eq!(s.confirm(), None, "one change, once");
        s.set(
            change.clone(),
            Ok(TrustList {
                local_peer: Some(me),
                allowed: vec![them],
            }),
        );
        assert_eq!(s.outcome().0, Some(&TrustOutcome::Changed(change)));
        assert_eq!(s.entry(), "", "the trusted PeerId leaves the field");
        assert_eq!(s.in_flight(), None);
    }

    #[test]
    fn of_every_input_only_opening_and_confirming_ask_for_anything() {
        let (me, them) = (peer(), peer());
        let mut s = TrustSettings::default();
        let mut asked = Vec::new();
        for input in [
            TrustInput::Opened,
            TrustInput::EntryChanged(them.as_str().to_owned()),
            TrustInput::ProposeAllow,
            TrustInput::Cancel,
            TrustInput::ProposeAllow,
            TrustInput::ProposeRevoke(them.clone()),
            TrustInput::Confirm,
        ] {
            if matches!(input, TrustInput::EntryChanged(_)) {
                s.read(Ok(TrustList {
                    local_peer: Some(me.clone()),
                    allowed: Vec::new(),
                }));
            }
            asked.extend(s.input(input));
        }
        assert_eq!(
            asked,
            [
                Intent::ReadTrust,
                Intent::SetTrust(TrustChange {
                    peer: them,
                    allowed: true
                })
            ],
            "a revoke of an unlisted peer proposes nothing, so the allow stands"
        );
    }

    #[test]
    fn a_cancelled_proposal_changes_nothing() {
        let (me, them) = (peer(), peer());
        let mut s = read(&me, std::slice::from_ref(&them));
        s.propose_revoke(&them);
        assert!(s.pending().is_some());
        s.cancel();
        assert_eq!(s.pending(), None);
        assert_eq!(s.confirm(), None, "nothing left to confirm");
    }

    #[test]
    fn a_typed_value_that_is_not_a_trustable_peer_is_not_proposed() {
        let (me, them) = (peer(), peer());
        let mut s = read(&me, std::slice::from_ref(&them));
        for (typed, why) in [
            ("alice".to_owned(), EntryProblem::NotAPeerId),
            (me.as_str().to_owned(), EntryProblem::OwnIdentity),
            (them.as_str().to_owned(), EntryProblem::AlreadyTrusted),
        ] {
            s.entry_changed(typed);
            s.propose_allow();
            assert_eq!(s.pending(), None);
            assert_eq!(s.outcome().0, Some(&TrustOutcome::Entry(why)));
        }
    }

    #[test]
    fn only_a_listed_peer_can_be_proposed_for_removal() {
        let (me, them) = (peer(), peer());
        let mut s = read(&me, &[]);
        s.propose_revoke(&them);
        assert_eq!(s.pending(), None);
    }

    #[test]
    fn a_refused_change_keeps_the_list_and_says_why() {
        let (me, them) = (peer(), peer());
        let mut s = read(&me, &[]);
        s.entry_changed(them.as_str().to_owned());
        s.propose_allow();
        let Some(Intent::SetTrust(change)) = s.confirm() else {
            panic!("a change");
        };
        let (_, before) = s.outcome();
        s.set(change, Err(TrustProblem::Unavailable));
        assert_eq!(
            s.outcome(),
            (
                Some(&TrustOutcome::Problem(TrustProblem::Unavailable)),
                before + 1
            )
        );
        assert_eq!(s.list().map(|l| l.allowed.len()), Some(0));
        assert_eq!(s.entry(), them.as_str(), "the field keeps what was typed");
    }
}
