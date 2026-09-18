// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The AutoNAT v2 SERVER role: `AUTONAT.md` §7 as the runtime drives it.
//!
//! What the server is, in this substrate: the vendored
//! `autonat::v2::server::Behaviour` wrapped three times, innermost
//! first --
//!
//! - [`ProbeServer`], §7's dial-back target rule and three budgets,
//!   which the crate does not implement (SPIKE-004 F2);
//! - [`Attributing`] with `always(DialOrigin::AutonatProbe)`, so the
//!   dial-back -- the one dial AutoNAT v2 makes, and the SERVER's
//!   (`v2/server/behaviour.rs:124`) -- reaches the outbound gate under
//!   the origin `AUTONAT.md` §7 names, and is admitted or refused by
//!   the root policy like every other behaviour dial. This is CLAUDE.md
//!   §1's ROUTE 1, reached for the first time by this step;
//! - [`ClassGated`] for the CONNECTIVITY-INFRASTRUCTURE service, so the
//!   dial-request protocol is offered to `DataPlaneTrusted` and
//!   `ConnectivityInfrastructureOnly` peers and to nobody else (§7:
//!   "only from peers admitted by its configured service policy").
//!
//! Constructed only when [`crate::SubstrateConfig::autonat_server`] is
//! `Some` -- the owner's 2026-09-07 ruling, gated off; `None` by
//! default, and the composition root (Stage 12) is where a profile's
//! `autonat.server.enabled` becomes a `Some`.
//!
//! # What this driver does
//!
//! Almost nothing, on purpose: the rules live in the wrapper, where the
//! events are. The driver translates the profile block, advances the
//! wrapper's clock each tick (its rate windows and in-flight horizon
//! read it), and turns the wrapper's events into the runtime's --
//! [`SwarmEvent::AutonatProbeServed`] and
//! [`SwarmEvent::AutonatProbeRefused`] -- so that a refusal is never a
//! counter nobody reads (CLAUDE.md §1 on invisible refusals; the
//! client driver's `ReachabilityReportRefused` is the model).
//!
//! # What retains a client's inbound
//!
//! A probe request arrives on an INBOUND connection, and an inbound
//! from a `ConnectivityInfrastructureOnly` peer is otherwise
//! established-then-closed (`dialing.rs`, the inbound arm under
//! `DialOrigin::Manual`). With the server configured, that arm asks
//! `authorizes_for(class, AutonatProbe)` for every inbound instead --
//! the same relaxation route 3 made for a client's known servers,
//! widened to every authorized peer because §7 lets every authorized
//! peer ask. A `DataPlaneTrusted` client was retained already; an
//! `Unauthorized` one is refused by both. `tests/connectivity/tests/
//! autonat_server.rs` pins the three over real sockets.

use interweave_profile_config::connectivity::AutonatServerConfig;
use interweave_transport_api::TransportIdentity;
use interweave_transport_runtime::DialOrigin;
use libp2p::autonat::v2::server::Behaviour as Server;
use libp2p::swarm::behaviour::toggle::Toggle;

use super::messages::SwarmEvent;
use crate::attribution::{Attributing, DialAttribution, always};
use crate::class_gate::{ClassGated, Service};
use crate::probe_server::{ProbeBudgets, ProbeRefusal, ProbeServer, ProbeServerEvent};
use interweave_transport_runtime::SnapshotHandle;

/// The server field's type in the composed behaviour.
pub type ServerField = Toggle<ClassGated<Attributing<ProbeServer>>>;

/// The server role's settings: `AUTONAT.md` §7's three budgets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutonatServerSettings {
    /// Dial-backs in flight at once.
    pub max_concurrent_probes: usize,
    /// Probe starts one client may make per minute.
    pub max_probes_per_peer_per_minute: usize,
    /// Probe starts across all clients per minute.
    pub max_probes_global_per_minute: usize,
}

impl AutonatServerSettings {
    /// Translate the validated profile block. Infallible: every field
    /// is a bounded integer `profile-config` already ranged, and the
    /// crate has no knob of its own to set (`AUTONAT.md` §7, note of
    /// 2026-09-18: the `timeout` the block once carried reached
    /// nothing and was removed).
    #[must_use]
    pub fn from_profile(config: &AutonatServerConfig) -> Self {
        Self {
            max_concurrent_probes: config.max_concurrent_probes as usize,
            max_probes_per_peer_per_minute: config.max_probes_per_peer_per_minute as usize,
            max_probes_global_per_minute: config.max_probes_global_per_minute as usize,
        }
    }

    /// Refuse a zero budget: the wrapper would serve nothing and say
    /// so only through refusal events, which is a misconfiguration
    /// dressed as a working server.
    ///
    /// # Errors
    /// The budget that is zero.
    pub const fn validate(&self) -> Result<(), &'static str> {
        if self.max_concurrent_probes == 0 {
            return Err("autonat server: max_concurrent_probes is zero");
        }
        if self.max_probes_per_peer_per_minute == 0 {
            return Err("autonat server: max_probes_per_peer_per_minute is zero");
        }
        if self.max_probes_global_per_minute == 0 {
            return Err("autonat server: max_probes_global_per_minute is zero");
        }
        Ok(())
    }

    /// The wrapper's budgets.
    #[must_use]
    pub const fn budgets(&self) -> ProbeBudgets {
        ProbeBudgets {
            max_concurrent: self.max_concurrent_probes,
            per_client_per_minute: self.max_probes_per_peer_per_minute,
            global_per_minute: self.max_probes_global_per_minute,
        }
    }
}

impl Default for AutonatServerSettings {
    /// §7's defaults: 8, 2 and 60.
    fn default() -> Self {
        Self {
            max_concurrent_probes: 8,
            max_probes_per_peer_per_minute: 2,
            max_probes_global_per_minute: 60,
        }
    }
}

/// Build the server field: the vendored server under the three
/// wrappers, announcing `AutonatProbe` for its dial-back into
/// `attribution`, class-gated for the infrastructure service.
#[must_use]
pub fn build_behaviour(
    settings: &AutonatServerSettings,
    attribution: DialAttribution,
    policy: SnapshotHandle,
) -> ServerField {
    Toggle::from(Some(ClassGated::for_service(
        Attributing::new(
            ProbeServer::new(Server::default(), settings.budgets()),
            always(DialOrigin::AutonatProbe),
            attribution,
        ),
        policy,
        Service::ConnectivityInfrastructure,
    )))
}

/// The wrapper's counters, read through the field.
#[must_use]
pub fn counters(field: &ServerField) -> Option<&crate::probe_server::ProbeCounters> {
    field.as_ref().map(|gated| gated.inner().inner().counters())
}

/// Advance the wrapper's clock; a no-op when the server is off.
pub fn tick(field: &mut ServerField, now_ms: u64) {
    if let Some(gated) = field.as_mut() {
        gated.inner_mut().inner_mut().tick(now_ms);
    }
}

/// Translate one wrapper event into the runtime's vocabulary.
///
/// `None` for a peer the neutral grammar refuses, which cannot occur:
/// the class gate admitted the connection by classifying that identity
/// first, and a dial-back names the peer the request came from. Stated
/// rather than relied on -- the wrapper's counter has already moved.
#[must_use]
pub fn translate(event: ProbeServerEvent) -> Option<SwarmEvent> {
    Some(match event {
        ProbeServerEvent::Served {
            client,
            address,
            outcome,
            data_amount,
        } => SwarmEvent::AutonatProbeServed {
            client: TransportIdentity::parse(client.to_base58()).ok()?,
            address: address.to_string(),
            reached: matches!(outcome, crate::probe_server::ServedOutcome::Ok),
            data_amount,
        },
        ProbeServerEvent::Refused {
            client,
            address,
            reason,
        } => SwarmEvent::AutonatProbeRefused {
            client: TransportIdentity::parse(client.to_base58()).ok()?,
            address: address.map(|a| a.to_string()),
            reason: label(reason),
        },
    })
}

/// §9's `outcome` label for a refusal.
const fn label(reason: ProbeRefusal) -> &'static str {
    reason.label()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;

    #[test]
    fn the_profile_block_translates_field_for_field_and_a_zero_budget_is_refused() {
        let profile = AutonatServerConfig::default();
        let settings = AutonatServerSettings::from_profile(&profile);
        assert_eq!(settings, AutonatServerSettings::default(), "§7's defaults");
        assert_eq!(settings.validate(), Ok(()));
        for zeroed in [
            AutonatServerSettings {
                max_concurrent_probes: 0,
                ..AutonatServerSettings::default()
            },
            AutonatServerSettings {
                max_probes_per_peer_per_minute: 0,
                ..AutonatServerSettings::default()
            },
            AutonatServerSettings {
                max_probes_global_per_minute: 0,
                ..AutonatServerSettings::default()
            },
        ] {
            assert!(zeroed.validate().is_err());
        }
    }

    #[test]
    fn a_wrapper_event_becomes_a_runtime_event_with_the_label_the_contract_names() {
        // A REAL ed25519 identity: `PeerId::random()` is a random
        // multihash the neutral grammar refuses, and `translate` says so
        // by returning `None` -- the case the doc names as unreachable
        // in production, where every peer authenticated through Noise
        // carries an identity-multihash PeerId.
        let client = libp2p::identity::Keypair::generate_ed25519()
            .public()
            .to_peer_id();
        assert_eq!(
            translate(ProbeServerEvent::Refused {
                client: libp2p::PeerId::random(),
                address: None,
                reason: ProbeRefusal::ClientRate,
            }),
            None,
            "a non-canonical PeerId yields no event rather than a wrong one"
        );
        let addr: libp2p::Multiaddr = "/ip4/8.8.8.8/tcp/1".parse().expect("a literal");
        let refused = translate(ProbeServerEvent::Refused {
            client,
            address: Some(addr.clone()),
            reason: ProbeRefusal::NotGlobal,
        });
        assert_eq!(
            refused,
            Some(SwarmEvent::AutonatProbeRefused {
                client: TransportIdentity::parse(client.to_base58())
                    .expect("ed25519 ids are neutral"),
                address: Some(addr.to_string()),
                reason: "refused_not_global",
            })
        );
        let served = translate(ProbeServerEvent::Served {
            client,
            address: addr.clone(),
            outcome: crate::probe_server::ServedOutcome::Failed,
            data_amount: 7,
        });
        assert!(matches!(
            served,
            Some(SwarmEvent::AutonatProbeServed {
                reached: false,
                data_amount: 7,
                ..
            })
        ));
    }
}
