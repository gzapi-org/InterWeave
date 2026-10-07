// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The trust settings' state (`human-client-ui.md` §8): the allowlist as
//! the daemon last said, the `PeerId` being typed, and a change waiting for
//! the person's confirmation. Pure: the root carries the intents out and
//! feeds back what the daemon answered.
//!
//! A change is never made from a proposal. The person proposes -- the
//! typed `PeerId`, or a listed peer's removal -- the view shows the exact
//! `PeerId` and the scope, and only [`TrustSettings::confirm`] of the
//! change shown yields the intent that reaches the daemon. Nothing a message carries reaches
//! here: the inputs are the settings view's alone.

#[cfg(test)]
use interweave_human_client_api::TrustRow;
use interweave_human_client_api::{TrustList, TrustProblem, TrustSetFailure};
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
    /// The daemon made the change.
    Changed(TrustChange),
    /// The daemon did not confirm the change, which may have been made;
    /// the allowlist is read again.
    Unconfirmed(TrustChange),
    /// Reading or changing did nothing.
    Problem(TrustProblem),
    /// The list a change left to read again could not be read: it may
    /// not show that change. Never "nothing was changed", which may be
    /// false here.
    NotReadAgain(TrustProblem),
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
    /// Carry out the change on show, named as it was shown: it is made
    /// only while it is still the change waiting, so a press can never
    /// confirm a proposal that replaced the one on screen.
    Confirm(TrustChange),
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
    /// The list shown may not be the daemon's: a change was made, or may
    /// have been, and its list was not read back.
    reread: bool,
    /// The read on its way is that owed re-read.
    rereading: bool,
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
            TrustInput::Confirm(shown) => self.confirm(&shown),
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
        // Only against a list read, as a removal is: before it, neither
        // check below can be made (`an_allow_is_proposed_only_against_a_list_read`).
        let Some(list) = self.list.as_ref() else {
            return;
        };
        if let Some(why) = entry_problem(list, &peer) {
            self.say(TrustOutcome::Entry(why));
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
        if self.list.as_ref().is_some_and(|l| l.allows(peer)) {
            self.pending = Some(TrustChange {
                peer: peer.clone(),
                allowed: false,
            });
        }
    }

    /// The person confirmed `shown`, the change on screen: the intent that
    /// makes it, if it is still the one waiting. The only way to one.
    pub fn confirm(&mut self, shown: &TrustChange) -> Option<Intent> {
        if self.in_flight.is_some() || self.pending.as_ref() != Some(shown) {
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

    /// The daemon's answer to a read. A re-read a change left owed that
    /// fails says the list may be stale, never that nothing changed. A
    /// waiting change the new list makes moot -- an allow of a peer now
    /// listed or of this profile -- is dropped.
    pub fn read(&mut self, answer: Result<TrustList, TrustProblem>) {
        self.reading = false;
        let rereading = std::mem::take(&mut self.rereading);
        match answer {
            Ok(list) => {
                if self
                    .pending
                    .as_ref()
                    .is_some_and(|c| c.allowed && entry_problem(&list, &c.peer).is_some())
                {
                    self.pending = None;
                }
                self.list = Some(list);
            }
            Err(problem) if rereading => self.say(TrustOutcome::NotReadAgain(problem)),
            Err(problem) => self.say(TrustOutcome::Problem(problem)),
        }
    }

    /// The daemon's answer to `change`: the allowlist read back, or why
    /// not -- and whether the change was made, which is what the person is
    /// told: "nothing was changed" only when nothing was. A change made or
    /// possibly made whose list was not read back is read again
    /// ([`Self::take_reread`]). The typed `PeerId` is cleared once it is
    /// trusted.
    pub fn set(&mut self, change: TrustChange, answer: Result<TrustList, TrustSetFailure>) {
        if self.in_flight.as_ref() == Some(&change) {
            self.in_flight = None;
        }
        match answer {
            // The list read back is the daemon's, and another local
            // administrator may have changed the same peer in between: a
            // list that says otherwise is not reported as the change made.
            Ok(list) if list.allows(&change.peer) != change.allowed => {
                self.list = Some(list);
                self.reread = true;
                self.say(TrustOutcome::Unconfirmed(change));
            }
            Ok(list) => {
                self.made(&change);
                self.list = Some(list);
                self.say(TrustOutcome::Changed(change));
            }
            Err(TrustSetFailure::MadeNotReadBack(_)) => {
                self.made(&change);
                self.reread = true;
                self.say(TrustOutcome::Changed(change));
            }
            Err(TrustSetFailure::Unconfirmed(_)) => {
                self.reread = true;
                self.say(TrustOutcome::Unconfirmed(change));
            }
            Err(TrustSetFailure::NotMade(problem)) => self.say(TrustOutcome::Problem(problem)),
        }
    }

    fn made(&mut self, change: &TrustChange) {
        if change.allowed && self.entry.trim() == change.peer.as_str() {
            self.entry.clear();
        }
    }

    /// The read a change's answer left owed, once nothing else is on its
    /// way: the root asks after applying the daemon's answers.
    pub fn take_reread(&mut self) -> Option<Intent> {
        if !self.reread || self.reading || self.in_flight.is_some() {
            return None;
        }
        self.reread = false;
        self.reading = true;
        self.rereading = true;
        Some(Intent::ReadTrust)
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

/// Why `peer` is not one to allow against `list`, if it is not.
fn entry_problem(list: &TrustList, peer: &TransportIdentity) -> Option<EntryProblem> {
    if list.local_peer.as_ref() == Some(peer) {
        Some(EntryProblem::OwnIdentity)
    } else if list.allows(peer) {
        Some(EntryProblem::AlreadyTrusted)
    } else {
        None
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
            allowed: allowed.iter().cloned().map(TrustRow::added_here).collect(),
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
        assert_eq!(s.confirm(&change), Some(Intent::SetTrust(change.clone())));
        assert_eq!(s.confirm(&change), None, "one change, once");
        s.set(
            change.clone(),
            Ok(TrustList {
                local_peer: Some(me),
                allowed: vec![TrustRow::added_here(them)],
            }),
        );
        assert_eq!(s.outcome().0, Some(&TrustOutcome::Changed(change)));
        assert_eq!(s.take_reread(), None, "read back: nothing owed");
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
            TrustInput::Confirm(TrustChange {
                peer: them.clone(),
                allowed: true,
            }),
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
        let shown = TrustChange {
            peer: them,
            allowed: false,
        };
        assert_eq!(s.confirm(&shown), None, "nothing left to confirm");
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
        let shown = s.pending().cloned().expect("proposed");
        let Some(Intent::SetTrust(change)) = s.confirm(&shown) else {
            panic!("a change");
        };
        let (_, before) = s.outcome();
        s.set(
            change,
            Err(TrustSetFailure::NotMade(TrustProblem::Unavailable)),
        );
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

    #[test]
    fn a_confirmation_makes_only_the_change_it_was_shown() {
        let (me, x, y) = (peer(), peer(), peer());
        let mut s = read(&me, &[x.clone(), y.clone()]);
        s.propose_revoke(&x);
        let shown = s.pending().cloned().expect("x shown");
        // A removal of y pressed before the screen showed it, then the
        // confirmation of what the screen asked: x.
        s.propose_revoke(&y);
        assert_eq!(s.confirm(&shown), None, "x is no longer the change waiting");
        assert_eq!(
            s.pending().map(|c| &c.peer),
            Some(&y),
            "y waits, for its own confirmation"
        );
    }

    #[test]
    fn a_failed_change_says_nothing_changed_only_when_nothing_did() {
        let (me, them) = (peer(), peer());
        for (failure, said, reread) in [
            (
                TrustSetFailure::NotMade(TrustProblem::Unavailable),
                "problem",
                false,
            ),
            (
                TrustSetFailure::Unconfirmed(TrustProblem::Unavailable),
                "unconfirmed",
                true,
            ),
            (
                TrustSetFailure::MadeNotReadBack(TrustProblem::Unavailable),
                "changed",
                true,
            ),
        ] {
            let mut s = read(&me, &[]);
            s.entry_changed(them.as_str().to_owned());
            s.propose_allow();
            let shown = s.pending().cloned().expect("proposed");
            let Some(Intent::SetTrust(change)) = s.confirm(&shown) else {
                panic!("a change");
            };
            s.set(change.clone(), Err(failure));
            let got = match s.outcome().0 {
                Some(TrustOutcome::Problem(_)) => "problem",
                Some(TrustOutcome::Unconfirmed(c)) if *c == change => "unconfirmed",
                Some(TrustOutcome::Changed(c)) if *c == change => "changed",
                other => panic!("{other:?}"),
            };
            assert_eq!(got, said, "{failure:?}");
            assert_eq!(
                s.take_reread(),
                reread.then_some(Intent::ReadTrust),
                "{failure:?}: the list read again when it may be stale"
            );
        }
    }

    #[test]
    fn a_failed_reread_never_says_nothing_changed() {
        let (me, them) = (peer(), peer());
        for failure in [
            TrustSetFailure::MadeNotReadBack(TrustProblem::Unavailable),
            TrustSetFailure::Unconfirmed(TrustProblem::Unavailable),
        ] {
            let mut s = read(&me, std::slice::from_ref(&them));
            s.propose_revoke(&them);
            let shown = s.pending().cloned().expect("proposed");
            let Some(Intent::SetTrust(change)) = s.confirm(&shown) else {
                panic!("a change");
            };
            s.set(change, Err(failure));
            assert_eq!(s.take_reread(), Some(Intent::ReadTrust));
            s.read(Err(TrustProblem::Unavailable));
            assert_eq!(
                s.outcome().0,
                Some(&TrustOutcome::NotReadAgain(TrustProblem::Unavailable)),
                "{failure:?}: the list may be stale, and nothing says it was not changed"
            );
        }
    }

    #[test]
    fn an_allow_is_proposed_only_against_a_list_read() {
        let (me, them) = (peer(), peer());
        let mut s = TrustSettings::default();
        s.entry_changed(me.as_str().to_owned());
        s.propose_allow();
        assert_eq!(s.pending(), None, "nothing to check it against yet");
        // A waiting allow the list then makes moot is dropped.
        let mut s = read(&me, &[]);
        s.entry_changed(them.as_str().to_owned());
        s.propose_allow();
        assert!(s.pending().is_some());
        s.read(Ok(TrustList {
            local_peer: Some(me),
            allowed: vec![TrustRow::added_here(them)],
        }));
        assert_eq!(s.pending(), None, "already trusted now");
    }

    #[test]
    fn a_read_back_that_says_otherwise_is_not_reported_as_the_change() {
        let (me, them) = (peer(), peer());
        let mut s = read(&me, std::slice::from_ref(&them));
        s.propose_revoke(&them);
        let shown = s.pending().cloned().expect("proposed");
        let Some(Intent::SetTrust(change)) = s.confirm(&shown) else {
            panic!("a change");
        };
        // Another administrator allowed it again between the set and the
        // read-back.
        s.set(
            change.clone(),
            Ok(TrustList {
                local_peer: Some(me),
                allowed: vec![TrustRow::added_here(them)],
            }),
        );
        assert_eq!(s.outcome().0, Some(&TrustOutcome::Unconfirmed(change)));
        assert_eq!(s.take_reread(), Some(Intent::ReadTrust));
    }
}
