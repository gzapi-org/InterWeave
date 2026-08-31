// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! Provider health, derived from the routing view (§14).

use interweave_discovery_api::ProviderHealth;
use interweave_kademlia_control_api::{KademliaMode, RoutingView};

/// The provider's health right now.
///
/// The population logic is [`RoutingView::health`] — consumed, not
/// reimplemented, with the trust-capped population arithmetic of
/// [`RoutingView::effective_target`] behind it. What this adds is the lifecycle guard and the Stage 10
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
    saturation_valid: bool,
) -> ProviderHealth {
    if !started || stopped {
        return ProviderHealth::Unavailable;
    }
    if !view.can_become_healthy() {
        // No remote trusted peer means nobody to route with, whatever
        // the round count says; the clamp below cannot change that.
        return ProviderHealth::Unavailable;
    }
    // §9.3 makes saturation conditional on more than the round count:
    // the caller says whether its extra conjuncts hold. When they do
    // not, health is computed as if no rounds had accrued, so a view
    // that LOOKS saturated cannot rest while a targetable peer waits
    // outside the routing set. The backoff interval deliberately keeps
    // the real round count — pacing and health answer different
    // questions.
    let effective = if saturation_valid {
        *view
    } else {
        RoutingView {
            no_progress_rounds: 0,
            ..*view
        }
    };
    let population = effective.health(recent_queries_succeeded);
    if mode == KademliaMode::Server && population == ProviderHealth::Healthy {
        ProviderHealth::Degraded
    } else {
        population
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_saturation_does_not_rest() {
        let v = RoutingView {
            routing_peers: 1,
            target_routing_peers: 64,
            max_routing_peers: 256,
            remote_trusted_population: 3,
            no_progress_rounds: 5,
        };
        assert_eq!(
            provider_health(true, false, KademliaMode::Client, &v, true, true),
            ProviderHealth::Healthy,
            "with the conjuncts held, five quiet rounds are a resting state"
        );
        assert_eq!(
            provider_health(true, false, KademliaMode::Client, &v, true, false),
            ProviderHealth::Degraded,
            "with a conjunct failed, the same view is still warming"
        );
    }

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
            provider_health(false, false, KademliaMode::Client, &v, true, true),
            ProviderHealth::Unavailable,
            "not started is not operating"
        );
        assert_eq!(
            provider_health(true, true, KademliaMode::Client, &v, true, true),
            ProviderHealth::Unavailable,
            "stopped is not operating"
        );
    }

    #[test]
    fn server_mode_health_is_capped_at_degraded_this_stage() {
        let v = satisfied_view();
        assert_eq!(
            provider_health(true, false, KademliaMode::Client, &v, true, true),
            ProviderHealth::Healthy,
            "the identical view is healthy as a client"
        );
        assert_eq!(
            provider_health(true, false, KademliaMode::Server, &v, true, true),
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
            provider_health(true, false, KademliaMode::Server, &unavailable, true, true),
            ProviderHealth::Unavailable,
            "a server that cannot become healthy is unavailable, not merely degraded"
        );
    }
}
