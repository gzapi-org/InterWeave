// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The Activity's model side, fed by the hub: what the window shows
//! follows whether the network service runs, as well as what it sends.
//!
//! A change of running state starts a fresh model either way. Started:
//! the listing that follows is a new runtime's, and nothing the last one
//! showed carries over. Stopped: the window holds no rows and no Ready
//! session, so it shows the service as not running and offers nothing
//! that sends -- a kept model would still read Ready, show no notice, and
//! mark a Send in flight that no facade would ever answer (review F3).

use interweave_human_app_core::{ModelSide, Opener, Surface};

use crate::hub::ToView;

/// The store could not be listed for this view: what it holds stays
/// unshown until the view attaches again. A class, never content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ListingFailed;

/// The model side a view attached to the hub drives.
pub struct ViewSide<S: Surface, O: Opener> {
    side: ModelSide<S, O>,
    fresh: Box<dyn FnMut() -> ModelSide<S, O>>,
    /// Network access as the hub last said it.
    network: bool,
}

impl<S: Surface, O: Opener> ViewSide<S, O> {
    /// `fresh` makes a model side over the view: called now, and again at
    /// every change of the service's running state.
    pub fn new(mut fresh: impl FnMut() -> ModelSide<S, O> + 'static) -> Self {
        Self {
            side: fresh(),
            fresh: Box::new(fresh),
            network: true,
        }
    }

    /// Apply one message from the hub.
    ///
    /// # Errors
    /// [`ListingFailed`] when the hub says the store could not be listed,
    /// for the root to report.
    pub fn apply(&mut self, message: ToView) -> Result<(), ListingFailed> {
        match message {
            ToView::Running(running) => {
                self.side = (self.fresh)();
                self.side.daemon_seen(Some(running));
                // Access is the platform's, not the service's: a fresh
                // model keeps what the hub last said of it.
                self.side.network_access(self.network);
            }
            ToView::NetworkAccess(allowed) => {
                self.network = allowed;
                self.side.network_access(allowed);
            }
            ToView::Update(update) => self.side.apply(*update),
            ToView::ListingFailed => return Err(ListingFailed),
        }
        Ok(())
    }

    /// The model side, for the root's turn.
    pub const fn side_mut(&mut self) -> &mut ModelSide<S, O> {
        &mut self.side
    }

    /// The model side, to read.
    #[must_use]
    pub const fn side(&self) -> &ModelSide<S, O> {
        &self.side
    }
}

#[cfg(test)]
mod tests {
    use interweave_human_app_core::Update;
    use interweave_human_chat_protocol::{HumanChatV2, MessageKind};
    use interweave_human_client_api::{ClientEvent, Origin, Received, SessionState};
    use interweave_human_core::RowId;
    use interweave_human_ui_model::{SessionNotice, UiModel, ViewEvent};
    use interweave_profile_identity::ProfileIdentity;
    use interweave_transport_api::EndpointId;

    use super::*;

    struct Blank;

    impl Surface for Blank {
        fn render(&mut self, _model: &UiModel) {}

        fn take_events(&mut self, _model: &UiModel) -> Vec<ViewEvent> {
            Vec::new()
        }
    }

    impl Opener for Blank {
        fn open(&mut self, _destination: &str) {}
        fn ask_network_access(&mut self) {}
    }

    fn view_side() -> ViewSide<Blank, Blank> {
        ViewSide::new(|| ModelSide::new(Blank, Blank))
    }

    fn arrival() -> Received {
        let peer = ProfileIdentity::generate()
            .transport_identity()
            .expect("a peer id");
        Received {
            row: RowId::from_stored(1),
            origin: Origin::Direct {
                peer,
                endpoint: EndpointId::parse("human").expect("an endpoint"),
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
        }
    }

    fn model(side: &ViewSide<Blank, Blank>) -> &UiModel {
        side.side().model()
    }

    #[test]
    fn a_service_that_stops_leaves_no_ready_session_and_no_rows() {
        let mut side = view_side();
        side.apply(ToView::Running(true)).expect("applied");
        side.apply(ToView::Update(Box::new(Update::Client(
            ClientEvent::Session(SessionState::Ready { endpoint: None }),
        ))))
        .expect("applied");
        side.apply(ToView::Update(Box::new(Update::Received(arrival()))))
            .expect("applied");
        assert_eq!(
            model(&side).session_notice(),
            None,
            "the control: Ready, no notice"
        );
        assert_eq!(
            model(&side).conversations().len(),
            1,
            "the control: a row shown"
        );

        side.apply(ToView::Running(false)).expect("applied");
        assert_eq!(
            model(&side).session_notice(),
            Some(SessionNotice::NoDaemon),
            "the window says the service is not running"
        );
        assert!(model(&side).conversations().is_empty(), "and shows no row");
    }

    #[test]
    fn a_service_that_starts_again_starts_from_a_fresh_model() {
        let mut side = view_side();
        side.apply(ToView::Running(true)).expect("applied");
        side.apply(ToView::Update(Box::new(Update::Received(arrival()))))
            .expect("applied");
        side.apply(ToView::Running(true)).expect("applied");
        assert!(
            model(&side).conversations().is_empty(),
            "nothing carries over"
        );
    }

    #[test]
    fn withheld_network_access_outlives_a_fresh_model() {
        let mut side = view_side();
        side.apply(ToView::Running(true)).expect("applied");
        assert_eq!(
            model(&side).session_notice(),
            Some(SessionNotice::Reconnecting),
            "the control: access held"
        );
        side.apply(ToView::NetworkAccess(false)).expect("applied");
        assert_eq!(
            model(&side).session_notice(),
            Some(SessionNotice::NetworkDenied)
        );
        side.apply(ToView::Running(true)).expect("applied");
        assert_eq!(
            model(&side).session_notice(),
            Some(SessionNotice::NetworkDenied),
            "a fresh model still knows access is withheld"
        );
        side.apply(ToView::NetworkAccess(true)).expect("applied");
        assert_eq!(
            model(&side).session_notice(),
            Some(SessionNotice::Reconnecting)
        );
    }

    #[test]
    fn a_listing_that_failed_is_said_to_the_root() {
        let mut side = view_side();
        assert_eq!(side.apply(ToView::ListingFailed), Err(ListingFailed));
    }
}
