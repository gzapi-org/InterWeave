// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The model side: owns the [`UiModel`], applies the facade side's
//! [`Update`]s to it, drives a view through [`Surface`], and turns the
//! intents the view resolves into [`Command`]s.

use std::collections::HashSet;

use interweave_human_chat_protocol::is_allowed_link_scheme;
use interweave_human_ui_model::{ConversationKey, Intent, UiModel, ViewEvent};

use crate::protocol::{Command, Update};

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

/// The model side of the root.
pub struct ModelSide<S: Surface, O: Opener> {
    model: UiModel,
    surface: S,
    opener: O,
    in_flight: HashSet<InFlight>,
}

impl<S: Surface, O: Opener> ModelSide<S, O> {
    /// A model side over `surface`, opening activated links with `opener`.
    pub fn new(surface: S, opener: O) -> Self {
        Self {
            model: UiModel::new(),
            surface,
            opener,
            in_flight: HashSet::new(),
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
            }
            Update::UnreadListed(unread) => self.model.unread_listed(unread),
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
            Update::Done(command) | Update::Failed(command) => {
                self.in_flight.remove(&InFlight::of(&command));
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
        self.in_flight
            .insert(InFlight::of(&command))
            .then_some(command)
    }
}
