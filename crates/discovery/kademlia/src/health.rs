// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! Provider health, derived from the routing view (§14).

use interweave_discovery_api::ProviderHealth;
use interweave_kademlia_control_api::{KademliaMode, RoutingView};

/// The provider's health right now.
///
/// The population logic is [`RoutingView::health`] — consumed, not
/// reimplemented. What this adds is the lifecycle guard and the Stage 10
/// server-mode ceiling: strong reachability evidence (`autonat_verified_direct`
/// or `active_relay_reservation`, §14) cannot exist before Stage 11 builds
/// AutoNAT and Relay, so a server-mode provider reports at most `Degraded`
/// — the `server_reachability_unverified` state — no matter how good its
/// routing population looks. The ceiling is asserted by
/// `server_mode_health_is_capped_at_degraded_this_stage`; Stage 11 lifts
/// it by feeding real evidence in, not by deleting the cap.
pub(crate) fn provider_health(
    started: bool,
    stopped: bool,
    mode: KademliaMode,
    view: &RoutingView,
    recent_queries_succeeded: bool,
) -> ProviderHealth {
    if !started || stopped {
        return ProviderHealth::Unavailable;
    }
    let population = view.health(recent_queries_succeeded);
    if mode == KademliaMode::Server && population == ProviderHealth::Healthy {
        ProviderHealth::Degraded
    } else {
        population
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn satisfied_view() -> RoutingView {
        RoutingView {
            routing_peers: 2,
            target_routing_peers: 64,
            max_routing_peers: 256,
            remote_trusted_population: 2,
            no_progress_rounds: 0,
        }
    }

    #[test]
    fn lifecycle_gates_health() {
        let v = satisfied_view();
        assert_eq!(
            provider_health(false, false, KademliaMode::Client, &v, true),
            ProviderHealth::Unavailable,
            "not started is not operating"
        );
        assert_eq!(
            provider_health(true, true, KademliaMode::Client, &v, true),
            ProviderHealth::Unavailable,
            "stopped is not operating"
        );
    }

    #[test]
    fn server_mode_health_is_capped_at_degraded_this_stage() {
        let v = satisfied_view();
        assert_eq!(
            provider_health(true, false, KademliaMode::Client, &v, true),
            ProviderHealth::Healthy,
            "the identical view is healthy as a client"
        );
        assert_eq!(
            provider_health(true, false, KademliaMode::Server, &v, true),
            ProviderHealth::Degraded,
            "server mode without strong reachability evidence is degraded (§14), \
             and no strong evidence class exists before Stage 11"
        );
    }

    #[test]
    fn the_cap_does_not_hide_a_worse_state() {
        let unavailable = RoutingView {
            remote_trusted_population: 0,
            ..satisfied_view()
        };
        assert_eq!(
            provider_health(true, false, KademliaMode::Server, &unavailable, true),
            ProviderHealth::Unavailable,
            "a server that cannot become healthy is unavailable, not merely degraded"
        );
    }
}
