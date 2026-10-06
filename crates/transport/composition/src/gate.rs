// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton

//! The dial gate's state per allowlisted peer (`CONNECTIVITY.md` §19,
//! A 2026-10-06): what the operator reads to answer "why can B not
//! reach A". The deadlines are the substrate's (`SwarmRuntime::peer_gates`);
//! the last outcome is recorded here, from the events the driver sees.
//!
//! Bounded by the allowlist: an outcome is kept only for a peer on it,
//! and forgotten when the peer is revoked, so the map never holds more
//! peers than `MAX_ALLOWED_PEERS` however many others dial or are dialled.
//!
//! And the connectivity lines (`observability.md` §Logs, A 2026-10-06):
//! the gate's transitions under [`CONNECTIVITY_TARGET`], connect,
//! disconnect, a failed dial and a scheduled retry at `debug`, a
//! quarantine and a refused reconnect at `info`. A line names the remote
//! `PeerId` only for a peer on the allowlist ([`Who::Listed`]); any other
//! is its class alone ([`Who::Class`]), and no line has an address
//! field: every value here is a label, a number or a `PeerId`.

use std::collections::BTreeMap;

use interweave_transport_api::{DisconnectReason, PeerPath, TransportIdentity};
use interweave_transport_libp2p::{DialFailureClass, DialRefusal};
use interweave_transport_runtime::DialOrigin;
use interweave_trust_api::{InfrastructureSet, PeerTrustPolicy, TrustDecision};

/// The target the connectivity lines are written under.
pub const CONNECTIVITY_TARGET: &str = "interweave::connectivity";

/// Who a connectivity line names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Who<'a> {
    /// A peer on the allowlist, named by its `PeerId`: the operator put it
    /// there, and the daemon's log is the run-dir owner's.
    Listed(&'a TransportIdentity),
    /// Any other peer, by its bounded class only.
    Class(&'static str),
}

impl<'a> Who<'a> {
    /// How a line names `peer`: by its `PeerId` when the allowlist holds
    /// it, else `infrastructure` or `unlisted`.
    pub(crate) fn of(
        peer: &'a TransportIdentity,
        trust: &PeerTrustPolicy,
        infrastructure: &InfrastructureSet,
    ) -> Self {
        if trust.decide(peer) == TrustDecision::Allowed {
            Self::Listed(peer)
        } else if infrastructure.permits_control_connection(peer) {
            Self::Class("infrastructure")
        } else {
            Self::Class("unlisted")
        }
    }
}

/// One connectivity line, its peer named as [`Who`] allows.
macro_rules! connectivity {
    ($level:ident, $who:expr, $message:literal $(, $field:ident = $value:expr)*) => {
        match $who {
            Who::Listed(peer) => tracing::$level!(
                target: CONNECTIVITY_TARGET,
                peer = peer.as_str()
                $(, $field = $value)*,
                $message
            ),
            Who::Class(class) => tracing::$level!(
                target: CONNECTIVITY_TARGET,
                peer_class = class
                $(, $field = $value)*,
                $message
            ),
        }
    };
}

/// A connection to the peer came up.
pub(crate) fn connected(who: Who<'_>, path: PeerPath) {
    connectivity!(debug, who, "peer connected", path = path_label(path));
}

/// The peer's last connection went.
pub(crate) fn disconnected(who: Who<'_>, reason: DisconnectReason) {
    let reason = match reason {
        DisconnectReason::Policy => "policy",
        DisconnectReason::Closed => "closed",
    };
    connectivity!(debug, who, "peer disconnected", reason = reason);
}

/// A dial to the peer failed.
pub(crate) fn dial_failed(who: Who<'_>, class: DialFailureClass) {
    connectivity!(debug, who, "dial failed", class = class.label());
}

/// A failed dial scheduled the peer's next retry.
pub(crate) fn retry_scheduled(
    who: Who<'_>,
    origin: DialOrigin,
    attempt: u32,
    delay_ms: u64,
    peer_backoff: bool,
) {
    connectivity!(
        debug,
        who,
        "retry scheduled",
        origin = origin_label(origin),
        attempt = attempt,
        delay_ms = delay_ms,
        peer_backoff = peer_backoff
    );
}

/// An address of the peer answered with another identity.
pub(crate) fn quarantined(who: Who<'_>, for_ms: u64) {
    connectivity!(info, who, "address quarantined", for_ms = for_ms);
}

/// The reconnect round's dial to the peer was refused.
pub(crate) fn reconnect_refused(who: Who<'_>, class: DialFailureClass) {
    connectivity!(info, who, "reconnect refused", class = class.label());
}

const fn path_label(path: PeerPath) -> &'static str {
    match path {
        PeerPath::Direct => "direct",
        PeerPath::Relayed => "relayed",
    }
}

const fn origin_label(origin: DialOrigin) -> &'static str {
    match origin {
        DialOrigin::Manual => "manual",
        DialOrigin::ConnectionManager => "connection_manager",
        DialOrigin::DiscoveryReconnect => "discovery_reconnect",
        DialOrigin::KademliaQuery => "kademlia_query",
        DialOrigin::RelayReservation => "relay_reservation",
        DialOrigin::RelayCircuit => "relay_circuit",
        DialOrigin::AutonatProbe => "autonat_probe",
        DialOrigin::DcutrHolePunch => "dcutr_hole_punch",
    }
}

/// A peer's last dial outcome, the bounded class a row carries
/// (`CONNECTIVITY.md` §19). Never an address.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LastOutcome {
    /// A connection to it came up.
    Connected,
    /// A dial to it failed in the network.
    DialFailed,
    /// An address answered with another identity.
    IdentityMismatch,
    /// The gate, or this node's own handler, refused the dial.
    Denied,
}

impl LastOutcome {
    /// The class as a row names it.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Connected => "connected",
            Self::DialFailed => "dial_failed",
            Self::IdentityMismatch => "identity_mismatch",
            Self::Denied => "denied",
        }
    }

    /// The outcome a failed dial's class is.
    #[must_use]
    pub const fn of_class(class: DialFailureClass) -> Self {
        match class {
            DialFailureClass::IdentityMismatch => Self::IdentityMismatch,
            DialFailureClass::Denied(_)
            | DialFailureClass::Retention(_)
            | DialFailureClass::LocallyDenied => Self::Denied,
            DialFailureClass::NoKnownAddress | DialFailureClass::DialFailed => Self::DialFailed,
        }
    }

    /// The outcome a refused dial is: the gate's own refusals are
    /// `denied`; having no address to dial is not a refusal and records
    /// nothing.
    #[must_use]
    pub const fn of_refusal(refusal: &DialRefusal) -> Option<Self> {
        match refusal {
            DialRefusal::NoKnownAddress => None,
            DialRefusal::Policy(_) | DialRefusal::Retention(_) => Some(Self::Denied),
            DialRefusal::Backend(_) => Some(Self::DialFailed),
        }
    }
}

/// One allowlisted peer's row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerGateRow {
    /// The peer.
    pub peer: TransportIdentity,
    /// Whether any connection to it is open.
    pub connected: bool,
    /// Dials to it are refused until then, milliseconds since the Unix
    /// epoch.
    pub backoff_until_ms: Option<u64>,
    /// One of its addresses at least is quarantined until then.
    pub quarantined_until_ms: Option<u64>,
    /// How the last dial or connection to it ended, if any has since the
    /// runtime started.
    pub last_outcome: Option<LastOutcome>,
}

/// The last outcome per allowlisted peer.
#[derive(Debug, Default)]
pub(crate) struct LastOutcomes {
    by_peer: BTreeMap<TransportIdentity, LastOutcome>,
}

impl LastOutcomes {
    /// Record `outcome` for `peer`, if the allowlist holds it.
    pub(crate) fn record(
        &mut self,
        trust: &PeerTrustPolicy,
        peer: &TransportIdentity,
        outcome: LastOutcome,
    ) {
        if trust.decide(peer) == TrustDecision::Allowed {
            self.by_peer.insert(peer.clone(), outcome);
        }
    }

    /// Forget `peer`: it left the allowlist.
    pub(crate) fn forget(&mut self, peer: &TransportIdentity) {
        self.by_peer.remove(peer);
    }

    /// The last outcome recorded for `peer`.
    pub(crate) fn get(&self, peer: &TransportIdentity) -> Option<LastOutcome> {
        self.by_peer.get(peer).copied()
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.by_peer.len()
    }
}

/// The class each allowlisted peer's last reconnect was refused with, so
/// a refusal is logged when it changes rather than once a round.
/// Bounded as [`LastOutcomes`] is: allowlisted peers only.
#[derive(Debug, Default)]
pub(crate) struct ReconnectRefusals {
    by_peer: BTreeMap<TransportIdentity, &'static str>,
}

impl ReconnectRefusals {
    /// Whether `class` is news for `peer` -- recorded if the allowlist
    /// holds it. A peer off the allowlist is always news and never kept.
    pub(crate) fn changed(
        &mut self,
        trust: &PeerTrustPolicy,
        peer: &TransportIdentity,
        class: DialFailureClass,
    ) -> bool {
        if trust.decide(peer) != TrustDecision::Allowed {
            return true;
        }
        self.by_peer.insert(peer.clone(), class.label()) != Some(class.label())
    }

    /// Forget `peer`'s refusal: it connected, or left the allowlist.
    pub(crate) fn forget(&mut self, peer: &TransportIdentity) {
        self.by_peer.remove(peer);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use interweave_profile_identity::ProfileIdentity;
    use std::io::Write;
    use std::sync::{Arc, Mutex};

    /// What a subscriber at `debug` writes while `body` runs.
    fn captured(body: impl FnOnce()) -> String {
        #[derive(Clone, Default)]
        struct Sink(Arc<Mutex<Vec<u8>>>);
        impl Write for Sink {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0.lock().expect("unpoisoned").extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let sink = Sink::default();
        let writer = sink.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::DEBUG)
            .with_ansi(false)
            .with_writer(move || writer.clone())
            .finish();
        tracing::subscriber::with_default(subscriber, body);
        let bytes = sink.0.lock().expect("unpoisoned").clone();
        String::from_utf8(bytes).expect("utf-8")
    }

    #[test]
    fn a_line_names_an_allowlisted_peer_and_only_the_class_of_any_other() {
        let (listed, infra, stranger) = (peer(), peer(), peer());
        let trust = PeerTrustPolicy::new([listed.clone()]).expect("one peer");
        let infrastructure = InfrastructureSet::new([infra.clone()]).expect("one peer");
        let log = captured(|| {
            dial_failed(
                Who::of(&listed, &trust, &infrastructure),
                DialFailureClass::DialFailed,
            );
            quarantined(Who::of(&infra, &trust, &infrastructure), 1_800_000);
            retry_scheduled(
                Who::of(&stranger, &trust, &infrastructure),
                DialOrigin::Manual,
                2,
                60_000,
                true,
            );
        });
        let lines: Vec<&str> = log.lines().collect();
        assert_eq!(lines.len(), 3, "{log}");
        assert!(lines[0].contains(CONNECTIVITY_TARGET), "{log}");
        assert!(
            lines[0].contains(listed.as_str()) && lines[0].contains("class=\"dial_failed\""),
            "the allowlisted peer by its PeerId: {log}"
        );
        assert!(
            lines[1].contains("peer_class=\"infrastructure\"") && lines[1].contains("INFO"),
            "{log}"
        );
        assert!(
            lines[2].contains("peer_class=\"unlisted\"")
                && lines[2].contains("delay_ms=60000")
                && lines[2].contains("DEBUG"),
            "{log}"
        );
        for other in [&infra, &stranger] {
            assert!(
                !log.contains(other.as_str()),
                "a peer off the allowlist is never named: {log}"
            );
        }
    }

    #[test]
    fn a_reconnect_refusal_is_news_once_per_class() {
        let (listed, stranger) = (peer(), peer());
        let trust = PeerTrustPolicy::new([listed.clone()]).expect("one peer");
        let mut refusals = ReconnectRefusals::default();
        let backoff =
            DialFailureClass::Denied(interweave_transport_runtime::DialDenial::PeerBackoff);
        assert!(refusals.changed(&trust, &listed, backoff));
        assert!(
            !refusals.changed(&trust, &listed, backoff),
            "the same, again"
        );
        assert!(refusals.changed(&trust, &listed, DialFailureClass::NoKnownAddress));
        refusals.forget(&listed);
        assert!(refusals.changed(&trust, &listed, DialFailureClass::NoKnownAddress));
        assert!(refusals.changed(&trust, &stranger, backoff));
        assert!(refusals.changed(&trust, &stranger, backoff), "never kept");
        assert_eq!(refusals.by_peer.len(), 1);
    }

    fn peer() -> TransportIdentity {
        ProfileIdentity::generate()
            .transport_identity()
            .expect("peer id")
    }

    #[test]
    fn an_outcome_is_kept_only_for_an_allowlisted_peer_and_forgotten_with_it() {
        let (listed, stranger) = (peer(), peer());
        let trust = PeerTrustPolicy::new([listed.clone()]).expect("one peer");
        let mut outcomes = LastOutcomes::default();
        outcomes.record(&trust, &stranger, LastOutcome::DialFailed);
        assert_eq!(outcomes.len(), 0, "a peer off the allowlist takes no room");
        outcomes.record(&trust, &listed, LastOutcome::Connected);
        outcomes.record(&trust, &listed, LastOutcome::Denied);
        assert_eq!(outcomes.get(&listed), Some(LastOutcome::Denied), "the last");
        outcomes.forget(&listed);
        assert_eq!(outcomes.get(&listed), None);
    }

    #[test]
    fn a_class_maps_to_its_bounded_outcome() {
        use interweave_transport_runtime::DialDenial;
        assert_eq!(
            LastOutcome::of_class(DialFailureClass::Denied(DialDenial::PeerBackoff)).label(),
            "denied"
        );
        assert_eq!(
            LastOutcome::of_class(DialFailureClass::IdentityMismatch).label(),
            "identity_mismatch"
        );
        assert_eq!(
            LastOutcome::of_refusal(&DialRefusal::NoKnownAddress),
            None,
            "nothing to dial is not an outcome"
        );
    }
}
