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

use std::collections::BTreeMap;

use interweave_transport_api::TransportIdentity;
use interweave_transport_libp2p::{DialFailureClass, DialRefusal};
use interweave_trust_api::{PeerTrustPolicy, TrustDecision};

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

#[cfg(test)]
mod tests {
    use super::*;
    use interweave_profile_identity::ProfileIdentity;

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
