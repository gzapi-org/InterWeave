// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The bounded command/event port between the Kademlia provider and the
//! Swarm-owned driver.
//!
//! Kademlia here is **peer routing only** (ADR-0009). Two absences in this
//! module are the substance of that, and both are structural rather than
//! documented:
//!
//! - **No record command exists.** [`KademliaCommand`] has no variant for
//!   `put_record`, `get_record`, `start_providing`, or anything like them.
//!   The rule is not "the driver must not call those" — there is nothing
//!   to call. Any later record use requires a new ADR, and would show up
//!   here as a new variant that a reviewer cannot miss.
//! - **No dial command exists.** Iterative queries make the Swarm request
//!   dials, but those are behaviour-originated and pass the root
//!   `DialAdmissionGate` (ADR-0011). A `Dial` variant on this port would
//!   be a provider-owned dial, which is exactly what the gate exists to
//!   prevent.
//!
//! Nothing here touches libp2p, a Swarm, or a socket. The driver that
//! consumes these commands owns all of that.

#![forbid(unsafe_code)]

use interweave_discovery_api::CandidatePeer;
use interweave_transport_api::TransportIdentity;
use serde::{Deserialize, Serialize};

/// Ceiling on results one query may return (`max_results_per_query`).
pub const MAX_RESULTS_PER_QUERY: usize = 20;
/// Ceiling on the routing table (`max_routing_peers`).
pub const MAX_ROUTING_PEERS: usize = 1024;
/// Consecutive no-progress exploration rounds before saturation.
pub const SATURATION_ROUNDS: u32 = 3;
/// Cap on the backed-off exploration interval, in milliseconds.
pub const MAX_EXPLORATION_INTERVAL_MS: u64 = 15 * 60 * 1000;

/// Whether this node serves the DHT or only queries it.
///
/// Set explicitly at start rather than inferred. A client-mode peer is not
/// promised to be discoverable by targeted peer routing, and inferring the
/// mode would make that promise accidentally.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum KademliaMode {
    /// Query only; does not answer routing requests.
    Client,
    /// Serves routing requests as well as querying.
    Server,
}

/// Why a query was issued.
///
/// The class travels with the query because the budgets, the cooldowns and
/// the saturation logic all differ by class, and a driver that lost the
/// distinction would account for them together.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum QueryClass {
    /// Initial, self, or bucket refresh.
    Bootstrap,
    /// Lookup of an independently trusted PeerId with fresh server-capability
    /// evidence and no usable addresses.
    Targeted,
    /// Random-key exploration with bounded results.
    Exploration,
}

/// A command the provider issues to the driver.
///
/// Deliberately small and deliberately incomplete: see the module note on
/// what is absent and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "command")]
pub enum KademliaCommand {
    /// Set client or server mode. Explicit, never inferred.
    SetMode {
        /// The mode to enter.
        mode: KademliaMode,
    },
    /// Offer a peer for routing-table admission.
    ///
    /// An **offer**, not an insertion. The driver still applies address
    /// checks, trust policy, exact protocol-support evidence, and the
    /// table bounds — a hint never goes directly into the routing table.
    OfferRoutingPeer {
        /// The peer being offered.
        peer: TransportIdentity,
        /// Opaque addresses to try.
        addresses: Vec<String>,
    },
    /// Start one query of the given class.
    StartQuery {
        /// Which class, for budget and cooldown accounting.
        class: QueryClass,
        /// The 32-byte lookup key.
        ///
        /// A fixed array, so an exploration key cannot be the wrong width
        /// and a "key" cannot smuggle a payload — this is peer routing,
        /// and the key space is the identifier space.
        key: [u8; 32],
    },
    /// Stop new queries and settle in-flight work.
    Shutdown,
}

/// What the driver reports back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "event")]
pub enum KademliaEvent {
    /// A query returned peers, already normalized as discovery candidates.
    QueryResults {
        /// Which class produced them.
        class: QueryClass,
        /// The observed candidates, bounded by `max_results_per_query`.
        candidates: Vec<CandidatePeer>,
    },
    /// A peer entered the routing table.
    RoutingPeerAdded {
        /// The admitted peer.
        peer: TransportIdentity,
    },
    /// A peer left the routing table, by eviction or expiry.
    ///
    /// Removes only Kademlia provenance. Whether the aggregate peer
    /// survives is `DiscoveryManager`'s decision, since another provider
    /// may still support it.
    RoutingPeerRemoved {
        /// The departed peer.
        peer: TransportIdentity,
    },
    /// A query ended without results.
    QueryFailed {
        /// Which class.
        class: QueryClass,
        /// Bounded diagnostic class, not a free-form message.
        reason: QueryFailure,
    },
}

/// Why a query produced nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueryFailure {
    /// The query exceeded `query_timeout`.
    TimedOut,
    /// No routing peers were available to query.
    NoRoutingPeers,
    /// A concurrency or rate budget refused it.
    BudgetExhausted,
    /// The driver is shutting down.
    ShuttingDown,
}

/// The routing view the provider reasons about.
///
/// Holds counts, not peers: the provider needs to know whether the view is
/// satisfied, and giving it the membership would invite it to make routing
/// decisions the driver owns.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoutingView {
    /// Peers currently in the routing table.
    pub routing_peers: u32,
    /// Configured `target_routing_peers`.
    pub target_routing_peers: u32,
    /// Configured `max_routing_peers`.
    pub max_routing_peers: u32,
    /// Distinct remote trusted peers, excluding the local identity.
    pub remote_trusted_population: u32,
    /// Consecutive exploration rounds that admitted nothing new.
    pub no_progress_rounds: u32,
}

impl RoutingView {
    /// The runtime target, capped by what trust actually permits.
    ///
    /// `min(target, max, remote_trusted_population)`. The third term is the
    /// one that matters in practice: a profile trusting two peers cannot
    /// reach a target of 64 no matter how long it explores, and a view that
    /// ignored it would chase an unreachable number forever.
    #[must_use]
    pub const fn effective_target(&self) -> u32 {
        let mut t = self.target_routing_peers;
        if self.max_routing_peers < t {
            t = self.max_routing_peers;
        }
        if self.remote_trusted_population < t {
            t = self.remote_trusted_population;
        }
        t
    }

    /// Whether the routing table has reached its effective target.
    #[must_use]
    pub const fn is_target_satisfied(&self) -> bool {
        self.routing_peers >= self.effective_target()
    }

    /// Whether exploration has stopped finding anything.
    ///
    /// Saturation is a legitimate resting state, not a failure: a small
    /// trusted overlay runs out of peers to find, and treating that as
    /// unhealthy would keep a two-peer deployment permanently degraded.
    #[must_use]
    pub const fn is_saturated(&self) -> bool {
        self.no_progress_rounds >= SATURATION_ROUNDS
    }

    /// Whether Kademlia can ever become healthy with this trust population.
    ///
    /// With no remote trusted peers there is nobody to route with, and the
    /// provider reports unavailable after startup grace rather than
    /// retrying into an empty set.
    #[must_use]
    pub const fn can_become_healthy(&self) -> bool {
        self.remote_trusted_population > 0
    }

    /// The health this view implies, given recent query success.
    ///
    /// Healthy when target-satisfied **or** saturated, which is why
    /// saturation is modelled at all.
    #[must_use]
    pub const fn health(
        &self,
        recent_queries_succeeded: bool,
    ) -> interweave_discovery_api::ProviderHealth {
        use interweave_discovery_api::ProviderHealth;
        if !self.can_become_healthy() {
            return ProviderHealth::Unavailable;
        }
        if recent_queries_succeeded && (self.is_target_satisfied() || self.is_saturated()) {
            ProviderHealth::Healthy
        } else {
            ProviderHealth::Degraded
        }
    }

    /// The next exploration interval, backing off while nothing is found.
    ///
    /// Doubles per no-progress round from `base_ms`, capped at 15 minutes.
    /// Without this a two-peer overlay would run a useless 60-second
    /// exploration loop forever.
    #[must_use]
    pub const fn next_exploration_interval_ms(&self, base_ms: u64) -> u64 {
        let mut interval = base_ms;
        let mut round = 0;
        while round < self.no_progress_rounds {
            interval = interval.saturating_mul(2);
            if interval >= MAX_EXPLORATION_INTERVAL_MS {
                return MAX_EXPLORATION_INTERVAL_MS;
            }
            round += 1;
        }
        interval
    }
}

/// The cross-field rules the configuration must satisfy when enabled.
///
/// Stated as code beside the port that depends on them, because these are
/// hard validation errors rather than warnings and the driver's bounds
/// assume they already hold.
///
/// # Errors
/// Returns the first violated rule.
pub fn validate_limits(
    target_routing_peers: u32,
    max_routing_peers: u32,
    bootstrap_min_interval_ms: u64,
    bootstrap_refresh_interval_ms: u64,
    max_results_per_query: u32,
    kbucket_size: u32,
) -> Result<(), LimitViolation> {
    if target_routing_peers > max_routing_peers {
        return Err(LimitViolation::TargetAboveMax);
    }
    if bootstrap_refresh_interval_ms < bootstrap_min_interval_ms {
        return Err(LimitViolation::RefreshBelowMinimum);
    }
    if max_results_per_query > kbucket_size {
        return Err(LimitViolation::ResultsAboveBucket);
    }
    Ok(())
}

/// A violated Kademlia cross-field rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LimitViolation {
    /// `target_routing_peers > max_routing_peers`.
    TargetAboveMax,
    /// `bootstrap_refresh_interval < bootstrap_min_interval`.
    RefreshBelowMinimum,
    /// `max_results_per_query > kbucket_size`.
    ResultsAboveBucket,
}

impl core::fmt::Display for LimitViolation {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let s = match self {
            Self::TargetAboveMax => "target_routing_peers exceeds max_routing_peers",
            Self::RefreshBelowMinimum => {
                "bootstrap_refresh_interval is below bootstrap_min_interval"
            }
            Self::ResultsAboveBucket => "max_results_per_query exceeds kbucket_size",
        };
        f.write_str(s)
    }
}

impl core::error::Error for LimitViolation {}

#[cfg(test)]
mod tests {
    use super::*;
    use interweave_discovery_api::ProviderHealth;

    fn view(routing: u32, target: u32, max: u32, trusted: u32, stalled: u32) -> RoutingView {
        RoutingView {
            routing_peers: routing,
            target_routing_peers: target,
            max_routing_peers: max,
            remote_trusted_population: trusted,
            no_progress_rounds: stalled,
        }
    }

    #[test]
    fn the_effective_target_is_capped_by_the_trusted_population() {
        // The term that matters: a profile trusting two peers cannot reach
        // a target of 64 however long it explores.
        assert_eq!(view(0, 64, 256, 2, 0).effective_target(), 2);
        assert_eq!(view(0, 64, 256, 999, 0).effective_target(), 64);
        assert_eq!(view(0, 64, 32, 999, 0).effective_target(), 32);
        assert_eq!(view(0, 64, 256, 0, 0).effective_target(), 0);
    }

    #[test]
    fn a_small_trusted_overlay_reaches_its_target() {
        let v = view(2, 64, 256, 2, 0);
        assert!(v.is_target_satisfied());
        assert_eq!(v.health(true), ProviderHealth::Healthy);
    }

    #[test]
    fn saturation_is_a_resting_state_not_a_failure() {
        // Below target and still healthy: exploration ran out of peers to
        // find, which is not the same as being broken.
        let v = view(1, 64, 256, 8, SATURATION_ROUNDS);
        assert!(!v.is_target_satisfied());
        assert!(v.is_saturated());
        assert_eq!(v.health(true), ProviderHealth::Healthy);

        // One round short is still merely degraded.
        let v = view(1, 64, 256, 8, SATURATION_ROUNDS - 1);
        assert!(!v.is_saturated());
        assert_eq!(v.health(true), ProviderHealth::Degraded);
    }

    #[test]
    fn no_trusted_peers_means_unavailable_not_degraded() {
        let v = view(0, 64, 256, 0, 99);
        assert!(!v.can_become_healthy());
        assert_eq!(v.health(true), ProviderHealth::Unavailable);
    }

    #[test]
    fn failing_queries_degrade_even_a_satisfied_view() {
        let v = view(64, 64, 256, 999, 0);
        assert!(v.is_target_satisfied());
        assert_eq!(v.health(false), ProviderHealth::Degraded);
    }

    #[test]
    fn exploration_backs_off_and_is_capped() {
        let base = 60_000;
        assert_eq!(
            view(0, 8, 32, 8, 0).next_exploration_interval_ms(base),
            60_000
        );
        assert_eq!(
            view(0, 8, 32, 8, 1).next_exploration_interval_ms(base),
            120_000
        );
        assert_eq!(
            view(0, 8, 32, 8, 2).next_exploration_interval_ms(base),
            240_000
        );
        // Capped rather than growing without bound.
        assert_eq!(
            view(0, 8, 32, 8, 99).next_exploration_interval_ms(base),
            MAX_EXPLORATION_INTERVAL_MS
        );
        assert!(
            view(0, 8, 32, 8, 60).next_exploration_interval_ms(base) <= MAX_EXPLORATION_INTERVAL_MS
        );
    }

    #[test]
    fn cross_field_limits_are_checkable() {
        assert_eq!(validate_limits(64, 256, 300_000, 900_000, 20, 20), Ok(()));
        assert_eq!(
            validate_limits(512, 256, 300_000, 900_000, 20, 20),
            Err(LimitViolation::TargetAboveMax)
        );
        assert_eq!(
            validate_limits(64, 256, 900_000, 300_000, 20, 20),
            Err(LimitViolation::RefreshBelowMinimum)
        );
        assert_eq!(
            validate_limits(64, 256, 300_000, 900_000, 20, 8),
            Err(LimitViolation::ResultsAboveBucket)
        );
    }

    #[test]
    fn the_command_port_has_no_record_or_dial_variant() {
        // The rule is structural, so the check is too: every variant is
        // matched exhaustively, and adding a record or dial command would
        // fail to compile here before a reviewer ever saw it.
        let commands = [
            KademliaCommand::SetMode {
                mode: KademliaMode::Client,
            },
            KademliaCommand::OfferRoutingPeer {
                peer: TransportIdentity::parse(
                    "12D3KooWDpJ7As7BWAwRMfu1VU2WCqNjvq387JEYKDBj4kx6nXTN",
                )
                .expect("valid"),
                addresses: Vec::new(),
            },
            KademliaCommand::StartQuery {
                class: QueryClass::Exploration,
                key: [0u8; 32],
            },
            KademliaCommand::Shutdown,
        ];
        for c in &commands {
            match c {
                KademliaCommand::SetMode { .. }
                | KademliaCommand::OfferRoutingPeer { .. }
                | KademliaCommand::StartQuery { .. }
                | KademliaCommand::Shutdown => {}
            }
        }
        assert_eq!(commands.len(), 4, "the port gained a command");
    }

    #[test]
    fn commands_and_events_serialize_with_their_discriminants() {
        let c = KademliaCommand::StartQuery {
            class: QueryClass::Bootstrap,
            key: [1u8; 32],
        };
        let json = serde_json::to_value(&c).expect("ser");
        assert_eq!(json["command"], "start_query");
        assert_eq!(json["class"], "bootstrap");
        assert_eq!(
            serde_json::from_value::<KademliaCommand>(json).expect("de"),
            c
        );

        let e = KademliaEvent::QueryFailed {
            class: QueryClass::Targeted,
            reason: QueryFailure::TimedOut,
        };
        let json = serde_json::to_value(&e).expect("ser");
        assert_eq!(json["event"], "query_failed");
        assert_eq!(json["reason"], "timed_out");
    }

    #[test]
    fn a_query_key_is_exactly_thirty_two_bytes() {
        // A fixed array, so a key cannot be the wrong width and cannot
        // smuggle a payload: this is peer routing, and the key space is
        // the identifier space.
        let short: Vec<u8> = vec![0; 31];
        let json = serde_json::json!({
            "command": "start_query",
            "class": "exploration",
            "key": short
        });
        assert!(serde_json::from_value::<KademliaCommand>(json).is_err());
    }
}
