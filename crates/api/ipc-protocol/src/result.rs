// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The result each method answers with (`LOCAL-IPC.md` §Method catalogue's
//! Result column): `empty-result`, `send-result`,
//! `endpoints:directory-response`, `admin-status`, `endpoint-list` and
//! `set-enabled-result`.
//!
//! Each is built FROM the neutral port's answer, so the server never
//! assembles a result by hand, and each deserializes with the schema's
//! bounds, so a client never trusts a server's list to be the size the
//! contract says.

use std::collections::BTreeSet;

use interweave_local_client_api::{
    AdminStatus, EndpointAdminView, Generation, LeaseRecord, MAX_CLIENT_KIND_CHARS,
};
use interweave_transport_api::{
    ConnectivitySummary, EndpointDirectoryV1, EndpointId, Health, MAX_DIRECTORY_ENTRIES,
    TransportError, TransportIdentity,
};
use serde::{Deserialize, Serialize};

/// The most rows `admin.endpoints.list` carries
/// (`ipc/endpoint-list.schema.json` `maxItems`).
pub const MAX_ENDPOINT_ROWS: usize = 64;

/// `ipc/empty-result`: the `{}` of a method with nothing to say.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EmptyResult {}

/// `ipc/send-result`: the endpoint the remote resolved the send to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SendResult {
    /// Never absent: an accepted send resolved to exactly one endpoint.
    pub resolved_endpoint: EndpointId,
}

/// `endpoints:directory-response`: a remote peer's endpoint directory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DirectoryResult {
    /// At most 32, unique.
    pub endpoints: Vec<EndpointId>,
    /// The remote's suggested freshness, as sent: the full u32 is legal,
    /// and clamping is the receiver's.
    pub ttl_ms: u32,
    /// Diagnostic only; never used for freshness.
    pub generated_at_ms: u64,
}

impl From<EndpointDirectoryV1> for DirectoryResult {
    fn from(directory: EndpointDirectoryV1) -> Self {
        Self {
            endpoints: directory.endpoints,
            ttl_ms: directory.ttl_ms,
            generated_at_ms: directory.generated_at_ms,
        }
    }
}

impl<'de> Deserialize<'de> for DirectoryResult {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            endpoints: Vec<EndpointId>,
            ttl_ms: u32,
            generated_at_ms: u64,
        }
        let wire = Wire::deserialize(d)?;
        bounded_unique(&wire.endpoints, MAX_DIRECTORY_ENTRIES).map_err(serde::de::Error::custom)?;
        Ok(Self {
            endpoints: wire.endpoints,
            ttl_ms: wire.ttl_ms,
            generated_at_ms: wire.generated_at_ms,
        })
    }
}

/// `ipc/admin-status`: the administrative status view.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdminStatusResult {
    /// The runtime's aggregate health.
    pub health: Health,
    /// This profile's identity.
    pub peer: TransportIdentity,
    /// The full connectivity summary.
    pub connectivity: ConnectivitySummary,
    /// The IPC server's own counters.
    pub ipc: IpcCounters,
    /// Pre-authentication admission counters, when the binding has them.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "absent_or_pre_auth"
    )]
    pub pre_auth: Option<PreAuthCounters>,
}

impl AdminStatusResult {
    /// The port's view, with the counters only the IPC server can keep.
    /// `ipc.active_leases` is the port's count, never the server's.
    #[must_use]
    pub fn new(status: AdminStatus, counters: ServerCounters) -> Self {
        Self {
            health: status.health,
            peer: status.peer,
            connectivity: status.connectivity,
            ipc: IpcCounters {
                data_connections: counters.data_connections,
                admin_connections: counters.admin_connections,
                active_leases: u64::try_from(status.active_leases).unwrap_or(u64::MAX),
                cross_domain_capability_denied_total: counters.cross_domain_capability_denied_total,
                peer_credential_refused_total: counters.peer_credential_refused_total,
                events_dropped_total: None,
            },
            pre_auth: None,
        }
    }
}

/// What the IPC server counts itself.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ServerCounters {
    /// Open data-socket connections.
    pub data_connections: u64,
    /// Open admin-socket connections.
    pub admin_connections: u64,
    /// Requests refused for naming the other domain's method.
    pub cross_domain_capability_denied_total: u64,
    /// Connections refused on their peer credential.
    pub peer_credential_refused_total: u64,
}

/// `admin-status.ipc`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IpcCounters {
    /// Open data-socket connections.
    pub data_connections: u64,
    /// Open admin-socket connections.
    pub admin_connections: u64,
    /// Endpoints currently leased.
    pub active_leases: u64,
    /// Always emitted; absent on the wire reads as 0.
    #[serde(default)]
    pub cross_domain_capability_denied_total: u64,
    /// Always emitted; absent on the wire reads as 0.
    #[serde(default)]
    pub peer_credential_refused_total: u64,
    /// Events dropped from full client queues. NOT KEPT yet, so not
    /// emitted: absent says "not counted", where a 0 would say "none
    /// dropped", and only the first is true. The drops are the binding's
    /// session queues', and counting them is its own batch
    /// (architect-cto's ruling, relay seq 9633).
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "absent_or_count"
    )]
    pub events_dropped_total: Option<u64>,
}

/// `admin-status.pre_auth`: every field optional on the wire.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreAuthCounters {
    /// Sources with pending pre-authentication state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tracked_sources: Option<u64>,
    /// Pre-authentication attempts pending.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_total: Option<u64>,
    /// Peers with pending pre-authentication state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tracked_peers: Option<u64>,
}

/// `ipc/endpoint-list`: every configured endpoint and its runtime state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EndpointList {
    /// At most [`MAX_ENDPOINT_ROWS`], unique by id.
    pub endpoints: Vec<EndpointRow>,
}

impl EndpointList {
    /// The port's rows.
    ///
    /// # Errors
    /// [`TransportError::Internal`] for more rows than the contract
    /// carries, or a lease whose client kind is outside its bounds: a
    /// port that answered either broke its own contract, and the server
    /// answers that rather than truncating.
    pub fn from_views(views: Vec<EndpointAdminView>) -> Result<Self, TransportError> {
        if views.len() > MAX_ENDPOINT_ROWS {
            return Err(TransportError::Internal);
        }
        let endpoints = views
            .into_iter()
            .map(EndpointRow::try_from)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self { endpoints })
    }
}

impl<'de> Deserialize<'de> for EndpointList {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            endpoints: Vec<EndpointRow>,
        }
        let wire = Wire::deserialize(d)?;
        let ids: Vec<_> = wire.endpoints.iter().map(|row| row.id.clone()).collect();
        bounded_unique(&ids, MAX_ENDPOINT_ROWS).map_err(serde::de::Error::custom)?;
        Ok(Self {
            endpoints: wire.endpoints,
        })
    }
}

/// One `endpoint-list` row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EndpointRow {
    /// The endpoint.
    pub id: EndpointId,
    /// Whether it accepts traffic and may be claimed.
    pub enabled: bool,
    /// Whether it receives directed sends naming no endpoint.
    pub default: bool,
    /// Always `false`: every administrative change is a runtime overlay
    /// (ADR-0028).
    pub persisted: NotPersisted,
    /// Present while a connection holds the lease.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "absent_or_lease"
    )]
    pub lease: Option<LeaseRow>,
}

impl TryFrom<EndpointAdminView> for EndpointRow {
    type Error = TransportError;

    fn try_from(view: EndpointAdminView) -> Result<Self, TransportError> {
        Ok(Self {
            id: view.endpoint,
            enabled: view.enabled,
            default: view.default,
            persisted: NotPersisted,
            lease: view.lease.map(LeaseRow::try_from).transpose()?,
        })
    }
}

/// The literal `false` of `persisted`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NotPersisted;

impl Serialize for NotPersisted {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_bool(false)
    }
}

impl<'de> Deserialize<'de> for NotPersisted {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        if bool::deserialize(d)? {
            return Err(serde::de::Error::custom("persisted is always false"));
        }
        Ok(Self)
    }
}

/// A row's live lease.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeaseRow {
    /// The lease's epoch.
    pub epoch: Generation,
    /// The label its holder claimed under.
    #[serde(deserialize_with = "client_kind")]
    pub client_kind: String,
}

impl TryFrom<LeaseRecord> for LeaseRow {
    type Error = TransportError;

    fn try_from(lease: LeaseRecord) -> Result<Self, TransportError> {
        let chars = lease.client_kind.chars().count();
        if chars == 0 || chars > MAX_CLIENT_KIND_CHARS {
            return Err(TransportError::Internal);
        }
        Ok(Self {
            epoch: lease.epoch,
            client_kind: lease.client_kind,
        })
    }
}

/// `ipc/set-enabled-result`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetEnabledResult {
    /// The epoch disabling revoked, when a lease was live.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "absent_or_generation"
    )]
    pub revoked_epoch: Option<Generation>,
}

fn bounded_unique<T: Ord>(items: &[T], max: usize) -> Result<(), String> {
    if items.len() > max {
        return Err(format!("at most {max} entries, got {}", items.len()));
    }
    if items.iter().collect::<BTreeSet<_>>().len() != items.len() {
        return Err("entries must be unique".to_owned());
    }
    Ok(())
}

fn client_kind<'de, D: serde::Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    let kind = String::deserialize(d)?;
    if kind.is_empty() || kind.chars().count() > MAX_CLIENT_KIND_CHARS {
        return Err(serde::de::Error::custom(format!(
            "a client kind is 1..={MAX_CLIENT_KIND_CHARS} characters"
        )));
    }
    Ok(kind)
}

fn absent_or_count<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<u64>, D::Error> {
    u64::deserialize(d).map(Some)
}

fn absent_or_pre_auth<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<Option<PreAuthCounters>, D::Error> {
    PreAuthCounters::deserialize(d).map(Some)
}

fn absent_or_lease<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<LeaseRow>, D::Error> {
    LeaseRow::deserialize(d).map(Some)
}

fn absent_or_generation<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<Option<Generation>, D::Error> {
    Generation::deserialize(d).map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ep(id: &str) -> EndpointId {
        EndpointId::parse(id).expect("endpoint")
    }

    fn epoch() -> Generation {
        Generation::parse("AAAAAAAAAAAAAAAA").expect("generation")
    }

    #[test]
    fn a_row_says_not_persisted_and_carries_the_lease() {
        let view = EndpointAdminView {
            endpoint: ep("human"),
            enabled: true,
            default: true,
            lease: Some(LeaseRecord {
                endpoint: ep("human"),
                epoch: epoch(),
                client_kind: "human-client".into(),
                session_id: "s1".into(),
            }),
        };
        let list = EndpointList::from_views(vec![view]).expect("one row");
        assert_eq!(
            serde_json::to_value(&list).expect("ser"),
            json!({"endpoints": [{
                "id": "human", "enabled": true, "default": true, "persisted": false,
                "lease": {"epoch": "AAAAAAAAAAAAAAAA", "client_kind": "human-client"}
            }]})
        );
        let back: EndpointList =
            serde_json::from_value(serde_json::to_value(&list).expect("ser")).expect("de");
        assert_eq!(back, list);
    }

    /// The row this crate SENDS is bounded in characters too, so a session
    /// the data side admitted with a 64-character, 128-byte kind lists.
    #[test]
    fn a_lease_row_bounds_its_client_kind_in_characters() {
        let view = |kind: String| EndpointAdminView {
            endpoint: ep("human"),
            enabled: true,
            default: false,
            lease: Some(LeaseRecord {
                endpoint: ep("human"),
                epoch: epoch(),
                client_kind: kind,
                session_id: "s".into(),
            }),
        };
        assert!(EndpointList::from_views(vec![view("é".repeat(MAX_CLIENT_KIND_CHARS))]).is_ok());
        assert_eq!(
            EndpointList::from_views(vec![view("é".repeat(MAX_CLIENT_KIND_CHARS + 1))]),
            Err(TransportError::Internal)
        );
        // And the lower bound: an empty kind is not a label (`minLength: 1`).
        assert_eq!(
            EndpointList::from_views(vec![view(String::new())]),
            Err(TransportError::Internal)
        );
    }

    #[test]
    fn a_list_past_its_bound_or_repeating_an_id_is_refused_both_ways() {
        let view = |i: usize| EndpointAdminView {
            endpoint: ep(&format!("e{i}")),
            enabled: true,
            default: false,
            lease: None,
        };
        assert!(EndpointList::from_views((0..MAX_ENDPOINT_ROWS).map(view).collect()).is_ok());
        assert_eq!(
            EndpointList::from_views((0..=MAX_ENDPOINT_ROWS).map(view).collect()),
            Err(TransportError::Internal)
        );
        let row = json!({"id": "a", "enabled": true, "default": false, "persisted": false});
        assert!(
            serde_json::from_value::<EndpointList>(json!({"endpoints": [row.clone(), row]}))
                .is_err()
        );
        let persisted = json!({"endpoints": [
            {"id": "a", "enabled": true, "default": false, "persisted": true}
        ]});
        assert!(serde_json::from_value::<EndpointList>(persisted).is_err());
    }

    #[test]
    fn a_directory_past_32_or_repeating_is_refused() {
        let ids = |n: usize| (0..n).map(|i| format!("e{i}")).collect::<Vec<_>>();
        let doc = |endpoints: Vec<String>| json!({"endpoints": endpoints, "ttl_ms": 4_294_967_295_u64, "generated_at_ms": 0});
        assert!(serde_json::from_value::<DirectoryResult>(doc(ids(32))).is_ok());
        assert!(serde_json::from_value::<DirectoryResult>(doc(ids(33))).is_err());
        assert!(
            serde_json::from_value::<DirectoryResult>(doc(vec!["a".into(), "a".into()])).is_err()
        );
    }

    /// A counter nothing keeps is absent, not 0.
    #[test]
    fn the_status_names_no_dropped_count_it_does_not_keep() {
        use interweave_transport_api::{
            DirectInboundState, PathReadiness, PreferredPathPolicy, TransportIdentity,
        };
        let status = AdminStatus {
            health: Health::Healthy,
            peer: TransportIdentity::parse("12D3KooWDpJ7As7BWAwRMfu1VU2WCqNjvq387JEYKDBj4kx6nXTN")
                .expect("peer"),
            connectivity: ConnectivitySummary {
                direct_inbound: DirectInboundState::Unknown,
                relay_inbound: PathReadiness::Unavailable,
                active_relay_reservations: 0,
                target_relay_reservations: 0,
                active_relayed_peer_paths: 0,
                hole_punch_inflight: 0,
                preferred_path_policy: PreferredPathPolicy::DirectFirst,
                updated_at: 0,
            },
            active_leases: 0,
        };
        let json = serde_json::to_value(AdminStatusResult::new(status, ServerCounters::default()))
            .expect("ser");
        assert!(json["ipc"].get("events_dropped_total").is_none(), "{json}");
        assert!(
            json["ipc"].get("peer_credential_refused_total").is_some(),
            "the kept ones are"
        );
    }

    #[test]
    fn set_enabled_and_empty_results_have_their_shapes() {
        assert_eq!(
            serde_json::to_value(EmptyResult {}).expect("ser"),
            json!({})
        );
        assert!(serde_json::from_value::<EmptyResult>(json!({"x": 1})).is_err());
        assert_eq!(
            serde_json::to_value(SetEnabledResult {
                revoked_epoch: None
            })
            .expect("ser"),
            json!({})
        );
        assert!(
            serde_json::from_value::<SetEnabledResult>(json!({"revoked_epoch": null})).is_err()
        );
    }
}
