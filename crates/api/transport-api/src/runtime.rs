// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The transport runtime's neutral surface (`contracts/TRANSPORT.md`):
//! the runtime-wide operations and events, as types and a trait, with no
//! backend type in any signature (plan §15, "no libp2p types cross the
//! transport/local-client boundary").
//!
//! WHAT IS HERE AND WHAT IS NOT. The operations here are the runtime's
//! own -- identity, capabilities, health, connectivity, peer diagnostics,
//! its event stream, shutdown. The per-caller operations -- `join`,
//! `leave`, `subscriptions`, `broadcast`, `send`, `local_endpoint`,
//! `peer_endpoints` -- act as a local session and are the
//! `LocalDataSession` binding's (`contracts/LOCAL-CLIENT.md` §2), which
//! derives the source endpoint from the session's lease rather than
//! taking it from an argument.

use core::future::Future;

use serde::{Deserialize, Serialize};

use crate::ids::TransportIdentity;
use crate::status::{ConnectivitySummary, Health, PeerPath, TransportCapabilities, TransportError};

/// `local_identity()`: who this profile is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalIdentity {
    /// The stable transport identity.
    pub peer: TransportIdentity,
    /// Increments on deliberate identity rotation.
    pub identity_epoch: u64,
}

/// A part of the runtime `health()` reports on apart from the aggregate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Component {
    /// The backend's own task: listening, dialling, the protocols.
    Transport,
    /// The configured discovery providers, aggregated.
    Discovery,
}

/// One component's health.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComponentHealth {
    /// Which component.
    pub component: Component,
    /// Its state.
    pub health: Health,
}

/// `health()`: the aggregate and the component summaries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HealthReport {
    /// The worst component's state.
    pub aggregate: Health,
    /// Each component, once.
    pub components: Vec<ComponentHealth>,
}

impl HealthReport {
    /// A report whose aggregate is the worst of `components`, `healthy`
    /// when there are none.
    #[must_use]
    pub fn from_components(components: Vec<ComponentHealth>) -> Self {
        let aggregate = components
            .iter()
            .map(|c| c.health)
            .max_by_key(|h| match h {
                Health::Healthy => 0,
                Health::Degraded => 1,
                Health::Unavailable => 2,
            })
            .unwrap_or(Health::Healthy);
        Self {
            aggregate,
            components,
        }
    }
}

/// `peers()`: one peer this runtime holds a connection to.
///
/// The connected subset of `TRANSPORT.md`'s diagnostics: a peer and its
/// preferred path class. Trust state, last-observed time and discovery
/// provenance are not reported yet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerSummary {
    /// The peer.
    pub peer: TransportIdentity,
    /// How it is reached now.
    pub path: PeerPath,
}

/// Why a peer's path changed (`PeerPathChanged`'s `reason_class`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PathChangeReason {
    /// A direct connection joined a relayed one, not by a hole punch.
    DirectEstablished,
    /// A hole punch's direct connection held past the stability gate
    /// (`DCUTR.md` §7's `reason=dcutr`).
    Dcutr,
    /// The direct connection went and a relayed one remains.
    DirectLost,
}

/// A runtime-wide event (`TRANSPORT.md` §Events), in neutral terms.
///
/// Per LOGICAL peer, not per connection: a second connection to a
/// connected peer is a `PeerPathChanged` or nothing, never a second
/// `PeerConnected` (`CONNECTIVITY.md` §5). Message and lease events are
/// the session binding's, since each belongs to one local client.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum TransportEvent {
    /// A peer went from no usable connection to one.
    PeerConnected {
        /// The peer.
        peer: TransportIdentity,
        /// Its path.
        path: PeerPath,
        /// Local millisecond timestamp.
        observed_at: u64,
    },
    /// A connected peer's preferred path changed.
    PeerPathChanged {
        /// The peer.
        peer: TransportIdentity,
        /// The path it had.
        previous: PeerPath,
        /// The path it has.
        current: PeerPath,
        /// Why.
        reason: PathChangeReason,
        /// Local millisecond timestamp.
        observed_at: u64,
    },
    /// A peer's last usable connection closed.
    PeerDisconnected {
        /// The peer.
        peer: TransportIdentity,
        /// Local millisecond timestamp.
        observed_at: u64,
    },
    /// The connectivity summary changed.
    ConnectivityChanged {
        /// The summary as it now stands.
        summary: ConnectivitySummary,
    },
}

/// The transport runtime's own operations.
///
/// Futures are `Send` so a caller may drive them from any task. Every
/// query answers `BackendUnavailable` once the runtime has stopped.
pub trait TransportRuntime {
    /// `local_identity()`.
    fn local_identity(&self) -> LocalIdentity;

    /// What this backend and profile support; `max_payload_bytes` is the
    /// profile's effective limit.
    fn capabilities(&self) -> TransportCapabilities;

    /// `health()`.
    fn health(&self) -> impl Future<Output = Result<HealthReport, TransportError>> + Send;

    /// `connectivity()`: causes no probe, reservation or hole punch.
    fn connectivity(
        &self,
    ) -> impl Future<Output = Result<ConnectivitySummary, TransportError>> + Send;

    /// `peers()`.
    fn peers(&self) -> impl Future<Output = Result<Vec<PeerSummary>, TransportError>> + Send;

    /// The next runtime-wide event; `None` once the runtime has stopped.
    fn next_event(&mut self) -> impl Future<Output = Option<TransportEvent>> + Send;

    /// `shutdown(grace)`: stop taking work, answer what is in flight
    /// within the backend's bounded grace, stop.
    fn shutdown(self) -> impl Future<Output = Result<(), TransportError>> + Send;
}

#[cfg(test)]
mod tests {
    use super::{Component, ComponentHealth, HealthReport};
    use crate::status::Health;

    #[test]
    fn the_aggregate_is_the_worst_component_and_healthy_when_empty() {
        let c = |component, health| ComponentHealth { component, health };
        assert_eq!(
            HealthReport::from_components(vec![
                c(Component::Transport, Health::Healthy),
                c(Component::Discovery, Health::Degraded),
            ])
            .aggregate,
            Health::Degraded
        );
        assert_eq!(
            HealthReport::from_components(vec![
                c(Component::Transport, Health::Unavailable),
                c(Component::Discovery, Health::Degraded),
            ])
            .aggregate,
            Health::Unavailable
        );
        assert_eq!(
            HealthReport::from_components(Vec::new()).aggregate,
            Health::Healthy
        );
    }
}
