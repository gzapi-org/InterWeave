// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! Stage 11 step 8: DCUtR, as the runtime composes and reads it.
//!
//! The behaviour is the pinned `libp2p-dcutr` under `HolePunchScope`
//! (`hole_punch.rs`: section 13's attempt lifecycle, which the crate
//! has no notion of), under `Attributing` with `always(DcutrHolePunch)`
//! so every punch dial reaches the root gate announced (SPIKE-004
//! R12.4) and the policy judges its destination -- an infrastructure-
//! only one refused, D1 -- and under `ClassGated` for the DATA-PLANE
//! service, so a non-data-plane peer is offered no DCUtR handler at
//! all and no attempt begins toward it (`DCUTR.md` section 2).
//! Constructed only when `SubstrateConfig.dcutr` is `Some` -- the
//! owner's 2026-09-07 ruling, gated off -- and off by default.
//!
//! The settings are the profile's `transport.connectivity.dcutr`
//! block, minus its `enabled` (the switch is the `Some`) and with its
//! `direct_stability_period` carried but not yet read: the interval
//! before a punched direct path counts as preferred is step 9's, and
//! step 7 announces a path change the moment the set changes.

use interweave_profile_config::connectivity::{DCUTR_INFLIGHT_PER_PEER, DcutrConfig};
use interweave_transport_api::TransportIdentity;
use interweave_transport_runtime::{DialOrigin, SnapshotHandle};
use libp2p::dcutr;
use libp2p::swarm::ConnectionId;
use libp2p::swarm::behaviour::toggle::Toggle;
use libp2p::{Multiaddr, PeerId};

use super::messages::{HolePunchOutcome, SwarmEvent};
use crate::attribution::{Attributing, DialAttribution, always};
use crate::class_gate::ClassGated;
use crate::hole_punch::{
    Ending, HolePunchBudgets, HolePunchCounterHandle, HolePunchEvent, HolePunchScope,
};

/// The DCUtR field's type in the composed behaviour.
pub type DcutrField = Toggle<ClassGated<Attributing<HolePunchScope>>>;

/// The DCUtR settings: section 13's bounds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DcutrSettings {
    /// Attempts in flight across all peers.
    pub max_inflight: usize,
    /// Attempts in flight toward one peer; standard v1 allows one.
    pub max_inflight_per_peer: usize,
    /// How long a peer waits after a failed attempt.
    pub retry_cooldown_ms: u64,
    /// How long a punched direct path must hold before it is preferred
    /// -- carried for step 9, read by nothing yet.
    pub direct_stability_period_ms: u64,
}

impl DcutrSettings {
    /// Translate the validated profile block. Infallible: every field
    /// is a bounded integer the validator already ranged.
    #[must_use]
    pub fn from_profile(config: &DcutrConfig) -> Self {
        Self {
            max_inflight: config.max_inflight as usize,
            max_inflight_per_peer: config.max_inflight_per_peer as usize,
            retry_cooldown_ms: u64::from(config.retry_cooldown_ms),
            direct_stability_period_ms: u64::from(config.direct_stability_period_ms),
        }
    }

    /// Refuse what the wrapper could not honour: a zero ceiling, which
    /// declines every attempt and reports a working behaviour; a
    /// per-peer ceiling other than standard v1's one; a per-peer
    /// ceiling above the global one.
    ///
    /// # Errors
    /// The first rule broken, named.
    pub const fn validate(&self) -> Result<(), &'static str> {
        if self.max_inflight == 0 {
            return Err("dcutr: max_inflight is zero");
        }
        if self.max_inflight_per_peer != DCUTR_INFLIGHT_PER_PEER as usize {
            return Err("dcutr: max_inflight_per_peer must be 1 (standard v1)");
        }
        if self.max_inflight_per_peer > self.max_inflight {
            return Err("dcutr: max_inflight_per_peer exceeds max_inflight");
        }
        if self.retry_cooldown_ms == 0 {
            return Err("dcutr: retry_cooldown is zero");
        }
        Ok(())
    }

    /// The wrapper's budgets.
    #[must_use]
    pub const fn budgets(&self) -> HolePunchBudgets {
        HolePunchBudgets {
            max_inflight: self.max_inflight,
            max_inflight_per_peer: self.max_inflight_per_peer,
            cooldown_ms: self.retry_cooldown_ms,
        }
    }
}

impl Default for DcutrSettings {
    /// Section 13's defaults: 4, 1, five minutes, ten seconds.
    fn default() -> Self {
        Self::from_profile(&DcutrConfig::default())
    }
}

/// Build the DCUtR field: the crate under the attempt lifecycle, the
/// attribution and the data-plane class gate, and a handle on the
/// wrapper's counters.
#[must_use]
pub fn build_behaviour(
    settings: &DcutrSettings,
    local_peer: PeerId,
    attribution: DialAttribution,
    policy: SnapshotHandle,
) -> (DcutrField, HolePunchCounterHandle) {
    let scope = HolePunchScope::new(dcutr::Behaviour::new(local_peer), settings.budgets());
    let counters = scope.counter_handle();
    (
        Toggle::from(Some(ClassGated::new(
            Attributing::new(scope, always(DialOrigin::DcutrHolePunch), attribution),
            policy,
        ))),
        counters,
    )
}

/// Advance the wrapper's clock; a no-op when DCUtR is off.
pub fn tick(field: &mut DcutrField, now_ms: u64) {
    if let Some(gated) = field.as_mut() {
        gated.inner_mut().inner_mut().tick(now_ms);
    }
}

/// Offer this profile's bound listeners to the crate as candidates; a
/// no-op when DCUtR is off.
pub fn offer_listeners<'a>(field: &mut DcutrField, listeners: impl Iterator<Item = &'a Multiaddr>) {
    if let Some(gated) = field.as_mut() {
        let scope = gated.inner_mut().inner_mut();
        for address in listeners {
            let _ = scope.offer_listener(address);
        }
    }
}

/// Whether `connection`'s establishment ended an attempt -- read once
/// per connection the runtime is told of; false when DCUtR is off.
pub fn take_punched(field: &mut DcutrField, connection: ConnectionId) -> bool {
    field
        .as_mut()
        .is_some_and(|gated| gated.inner_mut().inner_mut().take_punched(connection))
}

/// Translate one wrapper event into the runtime's vocabulary.
///
/// `None` for a peer the neutral grammar refuses, which cannot occur:
/// the class gate admitted the connection by classifying that identity
/// first. Stated rather than relied on.
#[must_use]
pub fn translate(event: HolePunchEvent) -> Option<SwarmEvent> {
    let (peer, outcome) = match event {
        HolePunchEvent::Declined { peer, reason } => (
            peer,
            HolePunchOutcome::Declined {
                reason: reason.label(),
            },
        ),
        HolePunchEvent::Started { peer } => (peer, HolePunchOutcome::Started),
        HolePunchEvent::Ended { peer, ending } => (
            peer,
            match ending {
                Ending::Succeeded => HolePunchOutcome::Succeeded,
                Ending::Failed(detail) => HolePunchOutcome::Failed { detail },
                Ending::TimedOut => HolePunchOutcome::TimedOut,
                Ending::Abandoned => HolePunchOutcome::Abandoned,
            },
        ),
    };
    Some(SwarmEvent::HolePunch {
        peer: TransportIdentity::parse(peer.to_base58()).ok()?,
        outcome,
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;

    #[test]
    fn the_profile_block_translates_field_for_field_and_the_rules_refuse() {
        let settings = DcutrSettings::from_profile(&DcutrConfig::default());
        assert_eq!(
            settings,
            DcutrSettings {
                max_inflight: 4,
                max_inflight_per_peer: 1,
                retry_cooldown_ms: 300_000,
                direct_stability_period_ms: 10_000,
            },
            "section 13's defaults"
        );
        assert_eq!(settings.validate(), Ok(()));
        assert_eq!(settings.budgets(), HolePunchBudgets::default());
        for broken in [
            DcutrSettings {
                max_inflight: 0,
                ..DcutrSettings::default()
            },
            DcutrSettings {
                max_inflight_per_peer: 2,
                ..DcutrSettings::default()
            },
            DcutrSettings {
                max_inflight_per_peer: 0,
                ..DcutrSettings::default()
            },
            DcutrSettings {
                retry_cooldown_ms: 0,
                ..DcutrSettings::default()
            },
        ] {
            assert!(broken.validate().is_err(), "{broken:?}");
        }
    }

    #[test]
    fn a_wrapper_event_becomes_a_runtime_event_with_the_label_the_contract_names() {
        let peer = libp2p::identity::Keypair::generate_ed25519()
            .public()
            .to_peer_id();
        let id = TransportIdentity::parse(peer.to_base58()).expect("ed25519 ids are neutral");
        assert_eq!(
            translate(HolePunchEvent::Declined {
                peer,
                reason: crate::hole_punch::Decline::Cooldown
            }),
            Some(SwarmEvent::HolePunch {
                peer: id.clone(),
                outcome: HolePunchOutcome::Declined {
                    reason: "declined_cooldown"
                }
            })
        );
        assert_eq!(
            translate(HolePunchEvent::Ended {
                peer,
                ending: Ending::Failed("no".to_owned())
            }),
            Some(SwarmEvent::HolePunch {
                peer: id,
                outcome: HolePunchOutcome::Failed {
                    detail: "no".to_owned()
                }
            })
        );
        assert_eq!(
            translate(HolePunchEvent::Started {
                peer: PeerId::random()
            }),
            None,
            "a non-canonical PeerId yields no event rather than a wrong one"
        );
    }
}
