// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The model side: owns the [`UiModel`], applies the facade side's
//! [`Update`]s to it, drives a view through [`Surface`], and turns the
//! intents the view resolves into [`Command`]s.

use std::collections::{HashSet, VecDeque};

use interweave_human_chat_protocol::is_allowed_link_scheme;
use interweave_human_ui_model::{ConversationKey, Intent, UiModel, ViewEvent};

use crate::protocol::{Command, Failure, Update};

/// A view as the root drives it: show the model, then hand back what the
/// person did. `ui-slint`'s `View` is one; a test can script one.
pub trait Surface {
    /// Show `model`.
    fn render(&mut self, model: &UiModel);
    /// What the person did since the last call, resolved against `model`.
    /// A call returns at most up to the first draft edit, which the root
    /// applies before calling again.
    fn take_events(&mut self, model: &UiModel) -> Vec<ViewEvent>;
}

/// Opens a link a person activated, outside the client: a browser, a mail
/// program. It never reaches the facade.
pub trait Opener {
    /// Open `destination`, which is an allowlisted link.
    fn open(&mut self, destination: &str);
}

/// An action still being carried out, so a second press of the same one
/// is not sent twice: a render's "viewed" can raise `MarkRead` again
/// before the first one is answered, and a person can press Send twice.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum InFlight {
    /// One send per conversation at a time.
    Send(ConversationKey),
    /// Any other command, by its value.
    Other(Command),
}

impl InFlight {
    fn of(command: &Command) -> Self {
        match command {
            Command::Send { key, .. } => Self::Send(key.clone()),
            other => Self::Other(other.clone()),
        }
    }
}

/// Something that did not happen and that a person or a log should hear
/// about: the root shows or records it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Problem {
    /// A command could not be carried out; nothing changed.
    Command {
        /// The command.
        command: Command,
        /// Why.
        why: Failure,
    },
    /// The store's unread rows could not be listed again.
    UnreadNotListed(Failure),
}

/// How many problems wait for [`ModelSide::take_problems`], at most; past
/// it the oldest goes and is counted in [`ModelSide::problems_dropped`].
/// A `MarkRead` the store keeps refusing is raised again at every focused
/// render, so a root that never takes them must not grow this.
pub const PROBLEM_CAP: usize = 32;

/// The model side of the root.
pub struct ModelSide<S: Surface, O: Opener> {
    model: UiModel,
    surface: S,
    opener: O,
    in_flight: HashSet<InFlight>,
    problems: VecDeque<Problem>,
    problems_dropped: u64,
    hidden_unread: usize,
    hidden_other: usize,
}

impl<S: Surface, O: Opener> ModelSide<S, O> {
    /// A model side over `surface`, opening activated links with `opener`.
    pub fn new(surface: S, opener: O) -> Self {
        Self {
            model: UiModel::new(),
            surface,
            opener,
            in_flight: HashSet::new(),
            problems: VecDeque::new(),
            problems_dropped: 0,
            hidden_unread: 0,
            hidden_other: 0,
        }
    }

    /// Whether a transport daemon serves the profile, as the root sees
    /// it: the facade cannot tell a missing daemon from any other failed
    /// open, the root can.
    pub fn daemon_seen(&mut self, present: bool) {
        self.model.daemon_seen(present);
    }

    /// What did not happen since the last call, oldest first.
    pub fn take_problems(&mut self) -> Vec<Problem> {
        self.problems.drain(..).collect()
    }

    /// How many problems were dropped past [`PROBLEM_CAP`].
    #[must_use]
    pub const fn problems_dropped(&self) -> u64 {
        self.problems_dropped
    }

    /// How many stored rows the last listings could not decode for
    /// display: held in the store, never shown. The root says so.
    #[must_use]
    pub const fn hidden_rows(&self) -> usize {
        self.hidden_unread + self.hidden_other
    }

    fn problem(&mut self, problem: Problem) {
        self.problems.push_back(problem);
        while self.problems.len() > PROBLEM_CAP {
            self.problems.pop_front();
            self.problems_dropped += 1;
        }
    }

    /// The model.
    #[must_use]
    pub const fn model(&self) -> &UiModel {
        &self.model
    }

    /// The surface.
    #[must_use]
    pub const fn surface(&self) -> &S {
        &self.surface
    }

    /// The surface, to drive the platform's half of it (window focus).
    pub const fn surface_mut(&mut self) -> &mut S {
        &mut self.surface
    }

    /// Apply what the facade side did.
    pub fn apply(&mut self, update: Update) {
        match update {
            Update::Listed(listing) => {
                // Pending first: it is authoritative over held updates.
                self.model.pending_listed(listing.pending);
                self.model.unread_listed(listing.unread);
                self.model.kept_listed(listing.kept);
                self.hidden_unread = listing.undecodable_unread;
                self.hidden_other = listing.undecodable_other;
            }
            Update::UnreadListed { rows, undecodable } => {
                self.model.unread_listed(rows);
                // A relist reads the unread rows only: their count is
                // replaced, the start's count of the others stands.
                self.hidden_unread = undecodable;
            }
            Update::UnreadNotListed(why) => self.problem(Problem::UnreadNotListed(why)),
            Update::Received(received) => self.model.received(received),
            Update::Client(event) => self.model.client_event(event),
            Update::Diagnostics(diagnostics) => self.model.diagnostics_updated(diagnostics),
            Update::Sent {
                row,
                destination,
                envelope,
                at,
                ..
            } => self.model.sent(row, &destination, envelope, at),
            Update::SendRefused { key, draft, error } => {
                self.model.send_refused(key, draft, &error);
            }
            Update::Read(row) => self.model.read(row),
            Update::Kept { item, row } => self.model.kept(item, row),
            Update::Unkept(row) => self.model.unkept(row),
            Update::Done(command) => {
                self.in_flight.remove(&InFlight::of(&command));
            }
            Update::Failed { command, why } => {
                self.in_flight.remove(&InFlight::of(&command));
                if let (Command::Keep { item, .. }, Failure::CopyGone) = (&command, why) {
                    // The content is gone: stop offering what cannot work.
                    self.model.copy_gone(*item);
                }
                self.problem(Problem::Command { command, why });
            }
        }
    }

    /// Render the model, take what the person did, and return the commands
    /// it asks of the facade side. The view's queue is drained before this
    /// returns -- `ui-slint`'s queue bound holds only for a root that does.
    pub fn turn(&mut self) -> Vec<Command> {
        // One render, then takes until one comes back empty. The render
        // comes first because it is what lets the view resolve a
        // conversation shown with unread items in it; a draft edit taken
        // here reaches the screen at the next turn's render, and the view
        // never writes over text it is still holding.
        self.surface.render(&self.model);
        let mut commands = Vec::new();
        loop {
            let events = self.surface.take_events(&self.model);
            if events.is_empty() {
                return commands;
            }
            for event in events {
                match event {
                    ViewEvent::DraftChanged { key, draft } => {
                        self.model.draft_changed(key, draft);
                    }
                    ViewEvent::Intent(intent) => {
                        if let Some(command) = self.command_for(intent) {
                            commands.push(command);
                        }
                    }
                }
            }
        }
    }

    /// The command for `intent`, or `None` when it needs none -- a link is
    /// opened here -- or the same action is still in flight.
    fn command_for(&mut self, intent: Intent) -> Option<Command> {
        let command = match intent {
            Intent::OpenLink(destination) => {
                // The model raises it only for an allowlisted link a person
                // activated; checked again here, at the edge where the
                // link leaves the client.
                if is_allowed_link_scheme(&destination) {
                    self.opener.open(&destination);
                }
                return None;
            }
            Intent::Send { key, draft } => Command::Send { key, draft },
            Intent::MarkRead(row) => Command::MarkRead(row),
            Intent::Keep { item, from } => Command::Keep { item, from },
            Intent::Unkeep(row) => Command::Unkeep(row),
            Intent::Retry(row) => Command::Retry(row),
            Intent::Cancel(row) => Command::Cancel(row),
            Intent::Reopen => Command::Reopen,
            Intent::RecheckStorage => Command::RecheckStorage,
        };
        if !self.in_flight.insert(InFlight::of(&command)) {
            return None;
        }
        if let Command::Send { key, .. } = &command {
            // The answer may touch the composer only if it is not edited
            // after this press.
            self.model.send_pressed(key);
        }
        Some(command)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use interweave_human_store::RowId;

    struct Nothing;

    impl Surface for Nothing {
        fn render(&mut self, _model: &UiModel) {}
        fn take_events(&mut self, _model: &UiModel) -> Vec<ViewEvent> {
            Vec::new()
        }
    }

    impl Opener for Nothing {
        fn open(&mut self, _destination: &str) {}
    }

    #[test]
    fn a_relist_replaces_the_unread_count_and_keeps_the_others() {
        let mut side = ModelSide::new(Nothing, Nothing);
        side.apply(Update::Listed(crate::protocol::Listing {
            undecodable_unread: 2,
            undecodable_other: 1,
            ..crate::protocol::Listing::default()
        }));
        assert_eq!(side.hidden_rows(), 3);
        side.apply(Update::UnreadListed {
            rows: Vec::new(),
            undecodable: 0,
        });
        assert_eq!(side.hidden_rows(), 1, "the pending and kept count stands");
        side.apply(Update::UnreadListed {
            rows: Vec::new(),
            undecodable: 4,
        });
        assert_eq!(side.hidden_rows(), 5, "replaced, not added");
    }

    #[test]
    fn a_relist_that_could_not_be_read_is_a_problem() {
        let mut side = ModelSide::new(Nothing, Nothing);
        side.apply(Update::UnreadNotListed(Failure::StorageUnavailable));
        assert_eq!(
            side.take_problems(),
            [Problem::UnreadNotListed(Failure::StorageUnavailable)]
        );
    }

    #[test]
    fn the_daemon_seen_reaches_the_model() {
        use interweave_human_client_api::{ClientEvent, SessionState};
        use interweave_human_ui_model::SessionNotice;
        let mut side = ModelSide::new(Nothing, Nothing);
        side.apply(Update::Client(ClientEvent::Session(
            SessionState::Reconnecting {
                attempt: 1,
                next_at: 0,
            },
        )));
        side.daemon_seen(false);
        assert_eq!(side.model().session_notice(), Some(SessionNotice::NoDaemon));
        side.daemon_seen(true);
        assert_eq!(
            side.model().session_notice(),
            Some(SessionNotice::Reconnecting)
        );
    }

    #[test]
    fn problems_never_pass_the_cap_and_the_oldest_are_counted() {
        let mut side = ModelSide::new(Nothing, Nothing);
        let failed = |n: usize| Update::Failed {
            command: Command::MarkRead(RowId::from_stored(i64::try_from(n).expect("small"))),
            why: Failure::StorageUnavailable,
        };
        for n in 0..PROBLEM_CAP + 3 {
            side.apply(failed(n));
        }
        assert_eq!(side.problems_dropped(), 3);
        let problems = side.take_problems();
        assert_eq!(problems.len(), PROBLEM_CAP);
        assert_eq!(
            problems[0],
            Problem::Command {
                command: Command::MarkRead(RowId::from_stored(3)),
                why: Failure::StorageUnavailable
            },
            "the oldest three went"
        );
        assert!(side.take_problems().is_empty(), "taken means gone");
    }
}
