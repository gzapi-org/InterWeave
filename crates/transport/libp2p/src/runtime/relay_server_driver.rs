// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The Circuit Relay v2 SERVER role: `RELAY.md` §8 as the runtime
//! drives it.
//!
//! What the server is, in this substrate: the pinned `relay::Behaviour`
//! under [`ClassGated`] for the CONNECTIVITY-INFRASTRUCTURE service, so
//! the hop protocol -- reservations and circuits -- is offered to
//! `DataPlaneTrusted` and `ConnectivityInfrastructureOnly` peers and to
//! nobody else (§8: "only peers classified `DataPlaneTrusted` or
//! `ConnectivityInfrastructureOnly` may obtain reservations/circuits";
//! open anonymous relay service is not a standard-v1 mode) -- and,
//! within that, only while this profile holds a verified direct external
//! address ([`HopGated`], §8's rule of 2026-09-26). Not
//! `Attributing`: the server dials nothing. A circuit's far end is
//! reached over the connection the destination already holds to this
//! relay (the stop protocol on it), and a reservation rides the
//! requester's inbound -- which the inbound arm in `dialing.rs` RETAINS
//! for an infrastructure-only peer when this profile serves relays,
//! under `RelayReservation`, exactly as it does for AutoNAT clients
//! under `AutonatProbe` (route 3, widened once more).
//!
//! Constructed only when [`crate::SubstrateConfig::relay_server`] is
//! `Some` -- the owner's 2026-09-07 ruling, gated off; `None` by
//! default, and the composition root (Stage 12) is where a profile's
//! `relay.server.enabled` becomes a `Some`.
//!
//! # The crate's defaults are not §8's, and its per-peer ceilings admit one more
//!
//! SPIKE-004 F10 measured both. `relay::Config::default()` is 128
//! reservations (§8: 64), 4 per peer (1), 16 circuits (128), 120 s per
//! circuit (1 h) and 128 KiB per circuit (64 MiB) -- two of those break
//! a deployment rather than merely differ -- so every field is set from
//! the profile and none is left to the crate. And the crate refuses a
//! per-peer request when the peer's count is GREATER THAN the ceiling
//! (`behaviour.rs:568`, `:695`), so a ceiling of 1 admits two; the
//! global ceilings use `>=` and are exact. [`RelayServerSettings::
//! crate_config`] therefore hands the crate `per_peer - 1`, and the
//! profile's floor of 1 keeps that non-negative. Pinned by
//! `the_crate_is_configured_one_below_each_per_peer_ceiling` here and
//! by `tests/connectivity/tests/relay_server.rs` on the wire.
//!
//! # What has no site
//!
//! §8's `max_pending_control` names a bound the crate has no field for
//! (F10). What bounds control work instead is the crate's own
//! per-connection concurrency -- at most ten inbound hop streams in
//! flight per connection (`behaviour/handler.rs`,
//! `MAX_CONCURRENT_STREAMS_PER_CONNECTION`) -- times the connection
//! ceiling the root policy holds; the profile key is read and
//! recorded, not enforced, and `RELAY.md` §8's note says so.
//! The crate's rate limiters are kept at their defaults, which is what
//! §8 asks ("rate limiters should be used where supported"). They are
//! TOKEN BUCKETS, not rates (`libp2p-relay` 0.22.0 `behaviour.rs:125-160`,
//! `behaviour/rate_limiter.rs`): per peer a bucket of thirty refilled one
//! token per two minutes, per IP a bucket of sixty refilled one per
//! minute, for reservations and circuit sources alike -- so one address
//! gets sixty at once and then one a minute (`RELAY.md` §8, measured by
//! SPIKE-004 phase B's `ratelimit` row). An earlier version of this note
//! read them as "sixty per IP per minute".
//!
//! # Events
//!
//! Every event the crate emits is translated into
//! [`SwarmEvent::RelayServed`] with a [`RelayServerOutcome`] -- an
//! acceptance, a denial with its status, a loss, a circuit opened or
//! closed -- so `RELAY.md` §11's server counters have a source, and a
//! denial is never a counter nobody reads.

use interweave_profile_config::connectivity::RelayServerConfig;
use interweave_transport_api::TransportIdentity;
use interweave_transport_runtime::SnapshotHandle;
use libp2p::PeerId;
use libp2p::relay::{Behaviour as Server, Config as CrateConfig, Event as ServerEvent, Status};
use libp2p::swarm::behaviour::toggle::Toggle;
use std::time::Duration;

use super::messages::{RelayServerOutcome, SwarmEvent};
use crate::class_gate::{ClassGated, Service};
use crate::hop_gate::HopGated;
use crate::served_addresses::ServedAddresses;

/// The server field's type in the composed behaviour.
pub type ServerField = Toggle<ClassGated<HopGated<ServedAddresses<Server>>>>;

/// The crate's own bound on inbound hop streams in flight per
/// connection (`libp2p-relay` 0.22.0 `behaviour/handler.rs`,
/// `MAX_CONCURRENT_STREAMS_PER_CONNECTION`), restated: what bounds
/// control work in place of §8's `max_pending_control`.
pub const CRATE_STREAMS_PER_CONNECTION: usize = 10;

/// The server role's settings: `RELAY.md` §8's ceilings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelayServerSettings {
    /// Reservations held for others at once.
    pub max_reservations: usize,
    /// Reservations one peer may hold: exact, not one more.
    pub max_reservations_per_peer: usize,
    /// How long one reservation lasts.
    pub reservation_duration_ms: u64,
    /// Circuits carried at once.
    pub max_circuits: usize,
    /// Circuits one peer may be party to, as source OR destination --
    /// the crate counts both -- exact.
    pub max_circuits_per_peer: usize,
    /// How long one circuit may live.
    pub max_circuit_duration_ms: u64,
    /// Bytes one circuit may carry.
    pub max_circuit_bytes: u64,
}

impl RelayServerSettings {
    /// Translate the validated profile block. `max_pending_control` is
    /// read by `profile-config` and reaches nothing here (the module
    /// note).
    ///
    /// # Errors
    /// [`Self::validate`]'s.
    pub fn from_profile(config: &RelayServerConfig) -> Result<Self, &'static str> {
        let settings = Self {
            max_reservations: config.max_reservations as usize,
            max_reservations_per_peer: config.max_reservations_per_peer as usize,
            reservation_duration_ms: u64::from(config.reservation_duration_ms),
            max_circuits: config.max_circuits as usize,
            max_circuits_per_peer: config.max_circuits_per_peer as usize,
            max_circuit_duration_ms: u64::from(config.max_circuit_duration_ms),
            max_circuit_bytes: config.max_circuit_bytes,
        };
        settings.validate()?;
        Ok(settings)
    }

    /// Refuse a configuration the crate would misread: a zero ceiling
    /// (the crate denies everything, or -- for a per-peer ceiling --
    /// `0 - 1` has no meaning), a per-peer ceiling above its total, a
    /// zero duration, or a circuit duration the crate cannot hold.
    ///
    /// # Errors
    /// The first rule broken, named.
    pub const fn validate(&self) -> Result<(), &'static str> {
        if self.max_reservations == 0 || self.max_reservations_per_peer == 0 {
            return Err("relay server: a reservation ceiling is zero");
        }
        if self.max_reservations_per_peer > self.max_reservations {
            return Err("relay server: max_reservations_per_peer exceeds max_reservations");
        }
        if self.max_circuits == 0 || self.max_circuits_per_peer == 0 {
            return Err("relay server: a circuit ceiling is zero");
        }
        if self.max_circuits_per_peer > self.max_circuits {
            return Err("relay server: max_circuits_per_peer exceeds max_circuits");
        }
        if self.reservation_duration_ms == 0 || self.max_circuit_duration_ms == 0 {
            return Err("relay server: a duration is zero");
        }
        // The crate keeps the circuit duration as u32 seconds.
        if self.max_circuit_duration_ms / 1_000 > u32::MAX as u64 {
            return Err("relay server: max_circuit_duration exceeds what the crate can hold");
        }
        if self.max_circuit_bytes == 0 {
            return Err("relay server: max_circuit_bytes is zero");
        }
        Ok(())
    }

    /// The crate's configuration for these settings: every ceiling
    /// set, the per-peer ones one below the profile's because the
    /// crate admits one more than it is told (the module note), the
    /// crate's rate limiters kept.
    #[must_use]
    pub fn crate_config(&self) -> CrateConfig {
        let defaults = CrateConfig::default();
        CrateConfig {
            max_reservations: self.max_reservations,
            max_reservations_per_peer: self.max_reservations_per_peer.saturating_sub(1),
            reservation_duration: Duration::from_millis(self.reservation_duration_ms),
            max_circuits: self.max_circuits,
            max_circuits_per_peer: self.max_circuits_per_peer.saturating_sub(1),
            max_circuit_duration: Duration::from_millis(self.max_circuit_duration_ms),
            max_circuit_bytes: self.max_circuit_bytes,
            ..defaults
        }
    }
}

impl Default for RelayServerSettings {
    /// §8's defaults: 64, 1, 1 h; 128, 4, 1 h, 64 MiB.
    fn default() -> Self {
        Self {
            max_reservations: 64,
            max_reservations_per_peer: 1,
            reservation_duration_ms: 3_600_000,
            max_circuits: 128,
            max_circuits_per_peer: 4,
            max_circuit_duration_ms: 3_600_000,
            max_circuit_bytes: 64 * 1024 * 1024,
        }
    }
}

/// Build the server field: the crate's server, told only its direct
/// addresses, hop-gated on holding one, under the class gate for the
/// infrastructure service.
#[must_use]
pub fn build_behaviour(
    settings: &RelayServerSettings,
    local_peer: PeerId,
    policy: SnapshotHandle,
) -> ServerField {
    let mut server = Server::new(local_peer, settings.crate_config());
    // ADVERTISING HOP IS DECIDED IN ONE PLACE, and that place is not the
    // crate. Since `libp2p-relay` 0.22 the crate defaults to
    // `auto_status_change`, which enables hop while `external_addresses`
    // is non-empty -- ANY external address, a relay-derived circuit one
    // included, which a dual-role profile holds and can serve nobody
    // with -- and holds `Status::Disable` otherwise, silently. The gate
    // `RELAY.md` §8 asks for is narrower: a verified DIRECT address,
    // answered per request by `HopGated` above the crate. Two opinions
    // on one question would disagree exactly on the dual-role profile,
    // so the crate's is switched off: `set_status(Some(..))` clears
    // `auto_status_change` permanently (the crate gates the
    // external-address logic on it) and `HopGated` alone decides.
    //
    // As first built (step 6) the forced `Enable` was the whole answer
    // -- a configured relay served whether or not it held an address --
    // and SPIKE-004 phase B measured what that cost: address-less
    // reservations holding the ceiling (`hop_gate`'s module note).
    server.set_status(Some(Status::Enable));
    Toggle::from(Some(ClassGated::for_service(
        // Hop offered only while a verified direct address is held
        // (`RELAY.md` §8, `hop_gate`), refused per request below the
        // class gate -- which decides once per connection and so could
        // not refuse a renewal on one already open.
        HopGated::new(
            // Told only the direct external addresses (`RELAY.md` §8): a
            // dual-role profile's relay-derived ones would be handed to
            // its clients as nested circuits.
            ServedAddresses::new(server),
        ),
        policy,
        Service::ConnectivityInfrastructure,
    )))
}

/// Translate one crate event into the runtime's vocabulary. `None`
/// for a peer the neutral grammar refuses, which cannot occur: the
/// class gate admitted the connection by classifying that identity
/// first. Stated rather than relied on.
#[must_use]
#[allow(deprecated)]
pub fn translate(event: ServerEvent) -> Option<SwarmEvent> {
    let ident = |p: PeerId| TransportIdentity::parse(p.to_base58()).ok();
    let (peer, other, outcome) = match event {
        ServerEvent::ReservationReqAccepted {
            src_peer_id,
            renewed,
        } => (
            src_peer_id,
            None,
            if renewed {
                RelayServerOutcome::ReservationRenewed
            } else {
                RelayServerOutcome::ReservationAccepted
            },
        ),
        ServerEvent::ReservationReqDenied {
            src_peer_id,
            status,
        } => (
            src_peer_id,
            None,
            RelayServerOutcome::ReservationDenied {
                status: format!("{status:?}"),
            },
        ),
        ServerEvent::ReservationReqAcceptFailed { src_peer_id, error }
        | ServerEvent::ReservationReqDenyFailed { src_peer_id, error } => (
            src_peer_id,
            None,
            RelayServerOutcome::ReservationExchangeFailed {
                detail: error.to_string(),
            },
        ),
        ServerEvent::ReservationClosed { src_peer_id } => {
            (src_peer_id, None, RelayServerOutcome::ReservationClosed)
        }
        ServerEvent::ReservationTimedOut { src_peer_id } => {
            (src_peer_id, None, RelayServerOutcome::ReservationTimedOut)
        }
        ServerEvent::CircuitReqAccepted {
            src_peer_id,
            dst_peer_id,
        } => (
            src_peer_id,
            Some(dst_peer_id),
            RelayServerOutcome::CircuitAccepted,
        ),
        ServerEvent::CircuitReqDenied {
            src_peer_id,
            dst_peer_id,
            status,
        } => (
            src_peer_id,
            Some(dst_peer_id),
            RelayServerOutcome::CircuitDenied {
                status: format!("{status:?}"),
            },
        ),
        ServerEvent::CircuitReqOutboundConnectFailed {
            src_peer_id,
            dst_peer_id,
            error,
        } => (
            src_peer_id,
            Some(dst_peer_id),
            RelayServerOutcome::CircuitExchangeFailed {
                detail: error.to_string(),
            },
        ),
        ServerEvent::CircuitReqDenyFailed {
            src_peer_id,
            dst_peer_id,
            error,
        }
        | ServerEvent::CircuitReqAcceptFailed {
            src_peer_id,
            dst_peer_id,
            error,
        } => (
            src_peer_id,
            Some(dst_peer_id),
            RelayServerOutcome::CircuitExchangeFailed {
                detail: error.to_string(),
            },
        ),
        ServerEvent::CircuitClosed {
            src_peer_id,
            dst_peer_id,
            error,
        } => (
            src_peer_id,
            Some(dst_peer_id),
            RelayServerOutcome::CircuitClosed {
                detail: error.map(|e| e.to_string()),
            },
        ),
        // NEW IN `libp2p-relay` 0.22 (the 0.57 bump): the crate reports
        // its own reachability status changing, derived from whether it
        // holds an external address. It names no peer and no circuit,
        // so it cannot become a `RelayServed` -- that event is about
        // what this server did FOR somebody. Consumed rather than
        // translated: what this profile advertises as a relay is the
        // reservation manager's business (`RELAY.md` §4), and the
        // AutoNAT verdict is where its own reachability is decided
        // (`AUTONAT.md` §5), so a second, crate-derived opinion on the
        // same question would be a third source for a consumer to
        // reconcile. Matched by name rather than swept into a
        // catch-all, so the next variant this crate adds fails the
        // build here instead of being silently dropped.
        // UNREACHABLE WHILE `build_behaviour` FORCES THE STATUS, and
        // matched by name anyway so the next variant this crate adds
        // fails the build here rather than being swallowed. The crate
        // pushes this from one place only (`behaviour.rs:398-404`,
        // inside `determine_relay_status_from_external_address`), which
        // `set_status(Some(..))` permanently disables -- so it is not a
        // live consumption path, and an earlier version of this comment
        // read as though it were (review, PR #109).
        ServerEvent::StatusChanged { .. } => return None,
    };
    Some(SwarmEvent::RelayServed {
        peer: ident(peer)?,
        destination: match other {
            Some(p) => Some(ident(p)?),
            None => None,
        },
        outcome,
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::panic)]

    use super::*;

    #[test]
    fn from_profile_translates_section_8s_defaults_and_refuses_what_the_crate_would_misread() {
        let settings =
            RelayServerSettings::from_profile(&RelayServerConfig::default()).expect("valid");
        assert_eq!(settings, RelayServerSettings::default());
        let rows: [(RelayServerSettings, &str); 5] = [
            (
                RelayServerSettings {
                    max_reservations_per_peer: 0,
                    ..RelayServerSettings::default()
                },
                "reservation ceiling is zero",
            ),
            (
                RelayServerSettings {
                    max_reservations_per_peer: 65,
                    ..RelayServerSettings::default()
                },
                "exceeds max_reservations",
            ),
            (
                RelayServerSettings {
                    max_circuits_per_peer: 129,
                    ..RelayServerSettings::default()
                },
                "exceeds max_circuits",
            ),
            (
                RelayServerSettings {
                    reservation_duration_ms: 0,
                    ..RelayServerSettings::default()
                },
                "duration is zero",
            ),
            (
                RelayServerSettings {
                    max_circuit_bytes: 0,
                    ..RelayServerSettings::default()
                },
                "max_circuit_bytes is zero",
            ),
        ];
        for (settings, expected) in rows {
            let err = settings.validate().expect_err("refused");
            assert!(err.contains(expected), "{err} should name {expected}");
        }
    }

    #[test]
    fn the_crate_is_configured_one_below_each_per_peer_ceiling() {
        // The crate refuses on `>` for the per-peer ceilings and on `>=`
        // for the totals (SPIKE-004 F10), so the totals pass through and
        // the per-peer figures are handed over one below.
        let settings = RelayServerSettings {
            max_reservations: 7,
            max_reservations_per_peer: 3,
            max_circuits: 9,
            max_circuits_per_peer: 4,
            ..RelayServerSettings::default()
        };
        let config = settings.crate_config();
        assert_eq!(config.max_reservations, 7);
        assert_eq!(config.max_reservations_per_peer, 2);
        assert_eq!(config.max_circuits, 9);
        assert_eq!(config.max_circuits_per_peer, 3);
        assert_eq!(config.reservation_duration, Duration::from_secs(3_600));
        assert_eq!(config.max_circuit_duration, Duration::from_secs(3_600));
        assert_eq!(config.max_circuit_bytes, 64 * 1024 * 1024);
        // And the crate's own defaults are not section 8's, in both
        // directions -- which is why nothing is left to them.
        let crate_defaults = CrateConfig::default();
        assert_eq!(crate_defaults.max_reservations, 128);
        assert_eq!(crate_defaults.max_circuits, 16);
        assert_eq!(crate_defaults.max_circuit_bytes, 1 << 17);
        assert_eq!(
            crate_defaults.max_circuit_duration,
            Duration::from_secs(120)
        );
        // The rate limiters are kept: two for reservations, two for
        // circuits.
        assert_eq!(config.reservation_rate_limiters.len(), 2);
        assert_eq!(config.circuit_src_rate_limiters.len(), 2);
    }
}
