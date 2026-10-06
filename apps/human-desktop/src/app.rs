// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The window's side of the root: the model side over the view, fed by
//! the facade thread. One `pump` applies what the facade sent, renders,
//! takes what the person did -- draining the view's queue, which its bound
//! depends on -- and hands the commands over.

use interweave_human_app_core::{ModelSide, Opener, Problem, Surface, Update};
use interweave_human_transport_client::{ClientEvent, SessionState};
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
/// argument, no shell, so nothing in the link is ever interpreted. The
/// link is content: a failure is reported by its kind alone, never with
/// the link.
pub struct DesktopOpener {
    program: std::path::PathBuf,
    report: fn(&str),
    spawned: fn(u32),
}

impl DesktopOpener {
    /// The desktop's handler, failures to stderr.
    #[must_use]
    pub fn new() -> Self {
        Self::with(std::path::PathBuf::from("xdg-open"), |line| {
            eprintln!("human-desktop: {line}");
        })
    }

    /// `program` in place of `xdg-open`, failures to `report`: how a test
    /// sees what is run and what is said.
    #[must_use]
    pub const fn with(program: std::path::PathBuf, report: fn(&str)) -> Self {
        Self {
            program,
            report,
            spawned: |_| {},
        }
    }

    /// Told each handler's process id as it starts: how a test checks the
    /// process is reaped.
    #[must_use]
    pub const fn on_spawn(mut self, spawned: fn(u32)) -> Self {
        self.spawned = spawned;
        self
    }
}

impl Default for DesktopOpener {
    fn default() -> Self {
        Self::new()
    }
}

impl Opener for DesktopOpener {
    fn open(&mut self, destination: &str) {
        match std::process::Command::new(&self.program)
            .arg(destination)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
        {
            // Reaped on a thread of its own, so an opened link leaves no
            // defunct process behind for the window's lifetime.
            Ok(mut child) => {
                (self.spawned)(child.id());
                std::thread::spawn(move || {
                    let _ = child.wait();
                });
            }
            Err(e) => (self.report)(&format!("a link could not be opened: {}", e.kind())),
        }
    }
}

/// The window's side of the root.
pub struct App<S: Surface, O: Opener> {
    side: ModelSide<S, O>,
    facade: Option<FacadeThread>,
    listing_failed: bool,
    /// When the root was made, and whether it has turned yet: the session
    /// log says how long after start each state came.
    started: std::time::Instant,
    turned: bool,
}

impl<S: Surface, O: Opener> App<S, O> {
    /// The root over `surface`, fed by `facade`.
    pub fn new(surface: S, opener: O, facade: FacadeThread) -> Self {
        Self {
            side: ModelSide::new(surface, opener),
            facade: Some(facade),
            listing_failed: false,
            started: std::time::Instant::now(),
            turned: false,
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
        if !self.turned {
            self.turned = true;
            eprintln!("human-desktop: first turn at {} ms", self.elapsed_ms());
        }
        for message in facade.take() {
            match message {
                FromFacade::Update(update) => {
                    if let Update::Client(ClientEvent::Session(state)) = &*update {
                        eprintln!(
                            "human-desktop: session {} at {} ms",
                            session_class(state),
                            self.elapsed_ms()
                        );
                    }
                    self.side.apply(*update);
                }
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
        // Never the peer: the daemon audits every trust set itself
        // (LOCAL-CLIENT.md section 5), and the client's log says no more
        // than that one was asked.
        Command::ReadTrust => "read trust",
        Command::SetTrust(_) => "set trust",
    }
}

impl<S: Surface, O: Opener> App<S, O> {
    fn elapsed_ms(&self) -> u128 {
        self.started.elapsed().as_millis()
    }
}

/// A session state as the log names it: its class and attempt, never an
/// endpoint or anything a message carried.
fn session_class(state: &SessionState) -> String {
    match state {
        SessionState::Ready { .. } => "ready".to_owned(),
        SessionState::Reconnecting { attempt, .. } => format!("reconnecting, attempt {attempt}"),
        SessionState::Refused { problem } => format!("refused: {problem:?}"),
        SessionState::StorageDegraded => "storage degraded".to_owned(),
        SessionState::Closed => "closed".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use interweave_human_transport_client::{SessionProblem, SessionState};
    use interweave_transport_api::EndpointId;

    use super::session_class;

    /// The log names a state's class and attempt, and no endpoint.
    #[test]
    fn a_session_state_is_logged_as_its_class() {
        let endpoint = EndpointId::parse("human-secret-label").expect("an endpoint");
        let ready = session_class(&SessionState::Ready {
            endpoint: Some(endpoint),
        });
        assert_eq!(ready, "ready");
        assert_eq!(
            session_class(&SessionState::Reconnecting {
                attempt: 3,
                next_at: 99
            }),
            "reconnecting, attempt 3"
        );
        assert_eq!(
            session_class(&SessionState::Refused {
                problem: SessionProblem::EndpointInUse
            }),
            "refused: EndpointInUse"
        );
    }
}
