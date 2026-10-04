// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The window's side of the root: the model side over the view, fed by
//! the facade thread. One `pump` applies what the facade sent, renders,
//! takes what the person did -- draining the view's queue, which its bound
//! depends on -- and hands the commands over.

use interweave_human_app_core::{ModelSide, Opener, Problem, Surface};
use interweave_human_ui_model::{UiModel, ViewEvent};
use interweave_human_ui_slint::View;

use crate::facade_thread::{FacadeThread, FromFacade};

/// The view as the root drives it.
pub struct SlintSurface(pub View);

impl Surface for SlintSurface {
    fn render(&mut self, model: &UiModel) {
        self.0.render(model);
    }

    fn take_events(&mut self, model: &UiModel) -> Vec<ViewEvent> {
        self.0.take_events(model)
    }
}

/// Opens an activated link with the desktop's handler: `xdg-open`, one
/// argument, no shell, so nothing in the link is ever interpreted.
pub struct DesktopOpener;

impl Opener for DesktopOpener {
    fn open(&mut self, destination: &str) {
        if let Err(e) = std::process::Command::new("xdg-open")
            .arg(destination)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
        {
            // The link itself is content: it is never logged.
            eprintln!("human-desktop: a link could not be opened: {}", e.kind());
        }
    }
}

/// The window's side of the root.
pub struct App<S: Surface, O: Opener> {
    side: ModelSide<S, O>,
    facade: Option<FacadeThread>,
    listing_failed: bool,
}

impl<S: Surface, O: Opener> App<S, O> {
    /// The root over `surface`, fed by `facade`.
    pub fn new(surface: S, opener: O, facade: FacadeThread) -> Self {
        Self {
            side: ModelSide::new(surface, opener),
            facade: Some(facade),
            listing_failed: false,
        }
    }

    /// The model side.
    #[must_use]
    pub const fn side(&self) -> &ModelSide<S, O> {
        &self.side
    }

    /// The model side, to drive the surface's platform half.
    pub const fn side_mut(&mut self) -> &mut ModelSide<S, O> {
        &mut self.side
    }

    /// Whether the store could not be listed at start.
    #[must_use]
    pub const fn listing_failed(&self) -> bool {
        self.listing_failed
    }

    /// One turn: apply what the facade sent, render, take the person's
    /// actions until the view's queue is empty, and hand the commands to
    /// the facade. Problems are logged as classes, never content.
    pub fn pump(&mut self) {
        let Some(facade) = &self.facade else {
            return;
        };
        for message in facade.take() {
            match message {
                FromFacade::Update(update) => self.side.apply(*update),
                FromFacade::Daemon(present) => self.side.daemon_seen(present),
                FromFacade::ListingFailed => {
                    self.listing_failed = true;
                    eprintln!("human-desktop: the message store could not be listed");
                }
                FromFacade::FacadeFailed => {
                    eprintln!("human-desktop: the message store's pending rows could not be read");
                }
                FromFacade::Closed => {}
            }
        }
        for command in self.side.turn() {
            if !facade.command(command) {
                eprintln!("human-desktop: the transport side has stopped");
                break;
            }
        }
        for problem in self.side.take_problems() {
            match problem {
                Problem::Command { command, why } => {
                    eprintln!("human-desktop: {} failed: {why:?}", command_name(&command));
                }
                Problem::UnreadNotListed(why) => {
                    eprintln!("human-desktop: unread messages could not be listed: {why:?}");
                }
            }
        }
    }

    /// Close the session -- the lease is released, the daemon keeps
    /// running -- and wait for the facade thread, at most `wait`.
    pub fn close(&mut self, wait: std::time::Duration) -> bool {
        self.facade.take().is_none_or(|facade| facade.close(wait))
    }
}

/// A command's name for a log line: never its content.
const fn command_name(command: &interweave_human_app_core::Command) -> &'static str {
    use interweave_human_app_core::Command;
    match command {
        Command::Send { .. } => "send",
        Command::MarkRead(_) => "mark read",
        Command::Keep { .. } => "keep",
        Command::Unkeep(_) => "unkeep",
        Command::Retry(_) => "retry",
        Command::Cancel(_) => "cancel",
        Command::Reopen => "reopen",
        Command::RecheckStorage => "recheck storage",
    }
}
