// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! Where the facade's output meets the Activity, which comes and goes
//! while the Service holds the session (human-client-android.md, "Service
//! ownership and local session": an Activity destroyed or rotated does
//! not release the lease).
//!
//! At most one view is attached. A view that attaches is sent whether
//! the network service runs, then -- once the facade runs -- the store's
//! listing before any update, so it never applies an update to rows it
//! has not listed. With no view attached, updates are not kept: the store
//! holds every row a view will list when it attaches, and what a person
//! must hear while no view is open is a notice, not an update.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use interweave_human_app_core::{Command, Update};

use crate::notice::Notices;

/// How many messages wait for the view at most. A view that falls that
/// far behind is detached rather than waited for: the facade then keeps
/// draining, so notices keep coming, and the view lists the store again
/// when it re-attaches ([`ViewLink::lost`]). Bounded memory, as the
/// desktop's window queue is (CLAUDE.md section 6).
pub const TO_VIEW: usize = 256;

/// What a view is sent.
#[derive(Debug)]
pub enum ToView {
    /// Whether the network service runs: the Android counterpart of the
    /// desktop's "a daemon serves this profile". A view seeing `false`
    /// shows that, and holds no rows until a listing arrives.
    Running(bool),
    /// Whether the platform gives this app network access: `false` in
    /// the network-denied posture, where the store is shown and nothing
    /// is sent. Sent at an attach and at every change; it outlives a
    /// change of [`ToView::Running`].
    NetworkAccess(bool),
    /// An update for the model side, the first after an attach or a start
    /// being `Update::Listed`.
    Update(Box<Update>),
    /// The store could not be listed for this view: what it holds stays
    /// unshown until the view attaches again. A class, never content.
    ListingFailed,
}

struct Viewer {
    send: SyncSender<ToView>,
    woken: Arc<AtomicBool>,
    notify: Arc<dyn Fn() + Send + Sync>,
    /// The listing went out to this viewer: updates may follow.
    listed: bool,
}

impl Viewer {
    /// Send `message`; false when the view is gone or too far behind,
    /// either way to be detached.
    fn send(&self, message: ToView) -> bool {
        match self.send.try_send(message) {
            Ok(()) => {
                // Sent before the flag is read, and a take clears the flag
                // before it reads: a message is in that take or asks a
                // wake of its own.
                if !self.woken.swap(true, Ordering::SeqCst) {
                    (self.notify)();
                }
                true
            }
            Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => false,
        }
    }
}

struct Slot {
    viewer: Option<Viewer>,
    running: bool,
    /// Whether the platform gives this app network access, as the
    /// Service last said; access is assumed until it says otherwise.
    network: bool,
}

impl Default for Slot {
    fn default() -> Self {
        Self {
            viewer: None,
            running: false,
            network: true,
        }
    }
}

/// The process's one meeting point between the facade and the view.
pub struct Hub {
    slot: Mutex<Slot>,
    commands: Mutex<Option<tokio::sync::mpsc::UnboundedSender<Command>>>,
    focused: AtomicBool,
    notices: Notices,
}

impl Default for Hub {
    fn default() -> Self {
        Self::new()
    }
}

impl Hub {
    /// No view, no facade.
    #[must_use]
    pub fn new() -> Self {
        Self {
            slot: Mutex::new(Slot::default()),
            commands: Mutex::new(None),
            focused: AtomicBool::new(false),
            notices: Notices::default(),
        }
    }

    fn slot(&self) -> MutexGuard<'_, Slot> {
        // A panic while the slot was held left nothing half-written that
        // a reader could misread: each field is replaced whole.
        self.slot.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Attach a view, replacing any attached before (whose link reports
    /// [`ViewLink::lost`]). `notify` is called, from whichever thread has
    /// something for the view, when a message waits and no wake is
    /// outstanding: the view schedules a turn with it.
    pub fn attach(&self, notify: Arc<dyn Fn() + Send + Sync>) -> ViewLink {
        let (send, from) = mpsc::sync_channel(TO_VIEW);
        let woken = Arc::new(AtomicBool::new(false));
        let viewer = Viewer {
            send,
            woken: Arc::clone(&woken),
            notify,
            listed: false,
        };
        let mut slot = self.slot();
        // A new viewer starts from what runs now; the listing follows from
        // the facade's next turn.
        viewer.send(ToView::Running(slot.running));
        viewer.send(ToView::NetworkAccess(slot.network));
        slot.viewer = Some(viewer);
        ViewLink {
            from,
            woken,
            lost: false,
        }
    }

    /// The view's window gained or lost the person's focus: while it has
    /// it, an arrival is read there and asks no notice.
    pub fn set_focused(&self, focused: bool) {
        self.focused.store(focused, Ordering::SeqCst);
        if focused {
            self.notices.clear();
        }
    }

    /// Whether a view's window has the person's focus now.
    #[must_use]
    pub fn focused(&self) -> bool {
        self.focused.load(Ordering::SeqCst)
    }

    /// The notices waiting for the platform.
    #[must_use]
    pub const fn notices(&self) -> &Notices {
        &self.notices
    }

    /// Carry out `command` on the facade. False while no facade runs: the
    /// view then shows the service as not running, and offers nothing that
    /// sends one.
    #[must_use]
    pub fn command(&self, command: Command) -> bool {
        self.commands
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .is_some_and(|commands| commands.send(command).is_ok())
    }

    // --- the facade's side -------------------------------------------------

    /// The facade started (`Some`, with where its commands go) or stopped.
    pub(crate) fn set_running(
        &self,
        commands: Option<tokio::sync::mpsc::UnboundedSender<Command>>,
    ) {
        let running = commands.is_some();
        *self.commands.lock().unwrap_or_else(PoisonError::into_inner) = commands;
        let mut slot = self.slot();
        slot.running = running;
        if let Some(viewer) = slot.viewer.as_mut() {
            // Listed again from the new facade's first turn: rows the
            // last one showed may have changed while none ran.
            viewer.listed = false;
            if !viewer.send(ToView::Running(running)) {
                slot.viewer = None;
            }
        }
    }

    /// Whether the platform gives this app network access, as the
    /// Service sees it: said to the attached view, and to each later one.
    pub(crate) fn set_network_access(&self, allowed: bool) {
        let mut slot = self.slot();
        if slot.network == allowed {
            return;
        }
        slot.network = allowed;
        if let Some(viewer) = slot.viewer.as_ref()
            && !viewer.send(ToView::NetworkAccess(allowed))
        {
            slot.viewer = None;
        }
    }

    /// Whether the attached view, if any, still waits for its listing
    /// from a running facade.
    pub(crate) fn wants_listing(&self) -> bool {
        let slot = self.slot();
        slot.running && slot.viewer.as_ref().is_some_and(|v| !v.listed)
    }

    /// Hand the listing (or its failure) to the view that waits for it.
    pub(crate) fn listed(&self, listing: ToView) {
        let mut slot = self.slot();
        if let Some(viewer) = slot.viewer.as_mut() {
            viewer.listed = true;
            if !viewer.send(listing) {
                slot.viewer = None;
            }
        }
    }

    /// Hand `update` to a listed view; with none, it is dropped (the
    /// store keeps the rows) after a received message has been counted
    /// for a notice when no window has the person's focus.
    pub(crate) fn update(&self, update: Update) {
        if matches!(update, Update::Received(_)) && !self.focused() {
            self.notices.received();
        }
        let mut slot = self.slot();
        if let Some(viewer) = slot.viewer.as_ref().filter(|v| v.listed)
            && !viewer.send(ToView::Update(Box::new(update)))
        {
            slot.viewer = None;
        }
    }
}

/// A view's end of the hub.
pub struct ViewLink {
    from: Receiver<ToView>,
    woken: Arc<AtomicBool>,
    lost: bool,
}

impl ViewLink {
    /// What was sent since the last call, in order. Answers the
    /// outstanding wake, so the next message asks for another.
    #[must_use]
    pub fn take(&mut self) -> Vec<ToView> {
        self.woken.store(false, Ordering::SeqCst);
        let mut taken = Vec::new();
        loop {
            match self.from.try_recv() {
                Ok(message) => taken.push(message),
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.lost = true;
                    break;
                }
            }
        }
        taken
    }

    /// The hub detached this view -- another attached, or it fell
    /// [`TO_VIEW`] behind -- and sends it nothing more. A view that is
    /// still shown attaches again, and starts again from a listing.
    #[must_use]
    pub const fn lost(&self) -> bool {
        self.lost
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicUsize;

    use interweave_human_app_core::Listing;
    use interweave_human_client_api::Diagnostics;

    use super::*;

    fn counting() -> (Arc<AtomicUsize>, Arc<dyn Fn() + Send + Sync>) {
        let count = Arc::new(AtomicUsize::new(0));
        let inner = Arc::clone(&count);
        (
            count,
            Arc::new(move || {
                inner.fetch_add(1, Ordering::SeqCst);
            }),
        )
    }

    fn listed() -> ToView {
        ToView::Update(Box::new(Update::Listed(Listing::default())))
    }

    #[test]
    fn a_view_hears_whether_the_service_runs_then_its_listing_then_updates() {
        let hub = Hub::new();
        let (_, wake) = counting();
        let mut link = hub.attach(wake);
        assert!(matches!(
            link.take()[..],
            [ToView::Running(false), ToView::NetworkAccess(true)]
        ));
        assert!(!hub.wants_listing(), "nothing to list while none runs");

        let (commands, _rx) = tokio::sync::mpsc::unbounded_channel();
        hub.set_running(Some(commands));
        assert!(hub.wants_listing());
        // An update before the listing never reaches the view.
        hub.update(Update::Diagnostics(Diagnostics::default()));
        hub.listed(listed());
        hub.update(Update::Diagnostics(Diagnostics::default()));
        let taken = link.take();
        assert!(matches!(taken[0], ToView::Running(true)), "{taken:?}");
        assert!(
            matches!(&taken[1], ToView::Update(u) if matches!(**u, Update::Listed(_))),
            "{taken:?}"
        );
        assert!(
            matches!(&taken[2], ToView::Update(u) if matches!(**u, Update::Diagnostics(_))),
            "{taken:?}"
        );
        assert_eq!(taken.len(), 3, "the pre-listing update was not sent");
    }

    #[test]
    fn one_wake_per_take_however_many_messages_wait() {
        let hub = Hub::new();
        let (woken, wake) = counting();
        let mut link = hub.attach(wake);
        let (commands, _rx) = tokio::sync::mpsc::unbounded_channel();
        hub.set_running(Some(commands));
        hub.listed(listed());
        assert_eq!(
            woken.load(Ordering::SeqCst),
            1,
            "Running(false), then nothing taken"
        );
        let _ = link.take();
        hub.update(Update::Diagnostics(Diagnostics::default()));
        hub.update(Update::Diagnostics(Diagnostics::default()));
        assert_eq!(woken.load(Ordering::SeqCst), 2, "one wake for both");
    }

    #[test]
    fn a_view_that_falls_behind_is_detached_and_told() {
        let hub = Hub::new();
        let (_, wake) = counting();
        let mut link = hub.attach(wake);
        let (commands, _rx) = tokio::sync::mpsc::unbounded_channel();
        hub.set_running(Some(commands));
        hub.listed(listed());
        // Running(false), Running(true) and the listing are queued already.
        for _ in 0..TO_VIEW {
            hub.update(Update::Diagnostics(Diagnostics::default()));
        }
        let taken = link.take();
        assert_eq!(taken.len(), TO_VIEW, "the queue held exactly its bound");
        assert!(link.lost(), "and the view was dropped, not waited for");
        assert!(!hub.wants_listing(), "no viewer remains");
    }

    #[test]
    fn a_view_hears_a_change_of_network_access_once_and_a_later_view_hears_it_too() {
        let hub = Hub::new();
        let (_, wake) = counting();
        let mut link = hub.attach(wake);
        let _ = link.take();
        hub.set_network_access(false);
        hub.set_network_access(false);
        assert!(
            matches!(link.take()[..], [ToView::NetworkAccess(false)]),
            "said once, at the change"
        );
        let (_, wake) = counting();
        let mut later = hub.attach(wake);
        assert!(matches!(
            later.take()[..],
            [ToView::Running(false), ToView::NetworkAccess(false)]
        ));
    }

    #[test]
    fn a_second_view_replaces_the_first() {
        let hub = Hub::new();
        let (_, wake) = counting();
        let mut first = hub.attach(Arc::clone(&wake));
        let mut second = hub.attach(wake);
        let _ = first.take();
        assert!(first.lost());
        assert!(matches!(
            second.take()[..],
            [ToView::Running(false), ToView::NetworkAccess(true)]
        ));
        assert!(!second.lost());
    }

    fn arrival() -> Update {
        use interweave_human_chat_protocol::{HumanChatV2, MessageKind};
        use interweave_human_client_api::{Origin, Received};
        let peer = interweave_profile_identity::ProfileIdentity::generate()
            .transport_identity()
            .expect("a peer id");
        Update::Received(Received {
            row: interweave_human_core::RowId::from_stored(1),
            origin: Origin::Direct {
                peer,
                endpoint: interweave_transport_api::EndpointId::parse("human")
                    .expect("an endpoint"),
            },
            envelope: HumanChatV2 {
                v: 2,
                kind: MessageKind::Text,
                app_message_id: format!("{:032x}", 1),
                text: "hello".to_owned(),
                reply_to: None,
                sent_at_ms: None,
                from_endpoint: None,
            },
            received_at: 1,
        })
    }

    /// human-client-android.md, "Notifications": an arrival no focused
    /// window reads asks a notice; one a focused window reads does not;
    /// and focus withdraws what waited (review F4).
    #[test]
    fn an_arrival_asks_a_notice_only_while_no_window_has_focus() {
        let now = std::time::Duration::ZERO;
        let hub = Hub::new();
        hub.update(arrival());
        assert_eq!(
            hub.notices().wait_change(now),
            Some(1),
            "unfocused: noticed"
        );

        hub.set_focused(true);
        assert_eq!(
            hub.notices().wait_change(now),
            Some(0),
            "focus withdraws it"
        );
        hub.update(arrival());
        assert_eq!(hub.notices().wait_change(now), None, "focused: read there");

        hub.set_focused(false);
        hub.update(Update::Diagnostics(Diagnostics::default()));
        assert_eq!(
            hub.notices().wait_change(now),
            None,
            "the control: an update that is no arrival asks nothing"
        );
    }

    #[test]
    fn a_command_goes_nowhere_while_no_facade_runs() {
        let hub = Hub::new();
        assert!(!hub.command(Command::Reopen));
        let (commands, mut rx) = tokio::sync::mpsc::unbounded_channel();
        hub.set_running(Some(commands));
        assert!(hub.command(Command::Reopen));
        assert!(matches!(rx.try_recv(), Ok(Command::Reopen)));
        hub.set_running(None);
        assert!(!hub.command(Command::Reopen));
    }
}
