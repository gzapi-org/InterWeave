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
    AdminStatus, EndpointAdminView, Generation, IngressCounts, LeaseRecord, MAX_CLIENT_KIND_CHARS,
    PeerGateView, PeerOutcome, PreAuthCounts, TrustAdminView, TrustSource,
};
use interweave_transport_api::{
    ConnectivitySummary, EndpointDirectoryV1, EndpointId, Health, MAX_DIRECTORY_ENTRIES,
    TransportError, TransportIdentity,
};
use serde::{Deserialize, Serialize};

/// The most rows `admin.endpoints.list` carries
/// (`ipc/endpoint-list.schema.json` `maxItems`).
pub const MAX_ENDPOINT_ROWS: usize = 64;

/// Rows on one `ipc/trust-list` page: a full allowlist of 4096 is four
/// pages, and a page stays under the 128 KiB body with the first page's
/// `local_peer` -- 82-byte rows at 2.1 (architect-cto's ruling of
/// 2026-10-04, LOCAL-IPC.md `admin.trust.list`), 105-byte rows at 2.3,
/// which carry `source` -- each with its array comma and a 52-character
/// peer id; about 105 KiB a page.
/// `a_full_trust_page_of_the_largest_rows_fits_the_body`.
pub const MAX_TRUST_PAGE_ROWS: usize = 1024;

/// The minor from which an `endpoint-list` row says `persisted: true`
/// for a runtime that keeps the endpoint overlay (ADR-0028 A 2026-10-11;
/// `ipc/endpoint-list` 1.1.0, behind the minor under ADR-0017 A
/// 2026-10-07). Below it the 2.0 row is served unchanged, `persisted:
/// false`.
pub const ENDPOINT_PERSISTED_SINCE_MINOR: u64 = 5;

/// The minor from which a `trust-list` row is the 2.3 row: `persisted`
/// true with its `source` (ADR-0017 A 2026-10-07; `ipc/trust-list`
/// 1.1.0). Below it the 2.1 row is served unchanged.
pub const TRUST_SOURCE_SINCE_MINOR: u64 = 3;

/// Rows on one `ipc/peer-list` page: a row is up to 194 bytes, so 1024
/// would overflow the 128 KiB body and 512 is about 100 KB; a full
/// allowlist of 4096 is eight pages (architect-cto's ruling of
/// 2026-10-07, LOCAL-IPC.md `admin.peers.list`).
/// `a_full_page_of_the_largest_rows_fits_the_body`.
pub const MAX_PEER_PAGE_ROWS: usize = 512;

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

impl From<DirectoryResult> for EndpointDirectoryV1 {
    fn from(result: DirectoryResult) -> Self {
        Self {
            generated_at_ms: result.generated_at_ms,
            ttl_ms: result.ttl_ms,
            endpoints: result.endpoints,
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
    /// The post-authentication ingress limiters' state, when the binding
    /// has it.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "absent_or_ingress"
    )]
    pub ingress: Option<IngressCounters>,
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
            pre_auth: status.pre_auth.map(|counts| PreAuthCounters {
                tracked_sources: Some(u64::try_from(counts.tracked_sources).unwrap_or(u64::MAX)),
                pending_total: Some(u64::try_from(counts.pending).unwrap_or(u64::MAX)),
            }),
            ingress: status.ingress.map(|counts| IngressCounters {
                direct_tracked_peers: Some(
                    u64::try_from(counts.direct_tracked_peers).unwrap_or(u64::MAX),
                ),
                broadcast_tracked_peers: Some(
                    u64::try_from(counts.broadcast_tracked_peers).unwrap_or(u64::MAX),
                ),
            }),
        }
    }
}

impl From<AdminStatusResult> for AdminStatus {
    /// The port's view as a client reads it back. The IPC counters are
    /// the server's and have no field in the neutral view but
    /// `active_leases`, which the server took from the port.
    fn from(result: AdminStatusResult) -> Self {
        Self {
            health: result.health,
            peer: result.peer,
            connectivity: result.connectivity,
            active_leases: usize::try_from(result.ipc.active_leases).unwrap_or(usize::MAX),
            // A block missing either count is not the funnel's view.
            pre_auth: result.pre_auth.and_then(|counters| {
                Some(PreAuthCounts {
                    tracked_sources: usize::try_from(counters.tracked_sources?)
                        .unwrap_or(usize::MAX),
                    pending: usize::try_from(counters.pending_total?).unwrap_or(usize::MAX),
                })
            }),
            // Likewise: a block missing either lane is not the limiters'.
            ingress: result.ingress.and_then(|counters| {
                Some(IngressCounts {
                    direct_tracked_peers: usize::try_from(counters.direct_tracked_peers?)
                        .unwrap_or(usize::MAX),
                    broadcast_tracked_peers: usize::try_from(counters.broadcast_tracked_peers?)
                        .unwrap_or(usize::MAX),
                })
            }),
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
}

/// `admin-status.ingress`: every field optional on the wire.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IngressCounters {
    /// Peers the direct lane's ingress limiter tracks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub direct_tracked_peers: Option<u64>,
    /// Peers the broadcast lane's ingress limiter tracks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub broadcast_tracked_peers: Option<u64>,
}

/// `ipc/endpoint-list`: every configured endpoint and its runtime state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EndpointList {
    /// At most [`MAX_ENDPOINT_ROWS`], unique by id.
    pub endpoints: Vec<EndpointRow>,
}

impl EndpointList {
    /// The port's rows, in the row shape the connection's `minor`
    /// negotiated: below [`ENDPOINT_PERSISTED_SINCE_MINOR`] the 2.0 row,
    /// `persisted: false` byte for byte as before it, and from it each
    /// view's own `persisted`.
    ///
    /// # Errors
    /// [`TransportError::Internal`] for more rows than the contract
    /// carries, or a lease whose client kind is outside its bounds: a
    /// port that answered either broke its own contract, and the server
    /// answers that rather than truncating.
    pub fn from_views(views: Vec<EndpointAdminView>, minor: u64) -> Result<Self, TransportError> {
        if views.len() > MAX_ENDPOINT_ROWS {
            return Err(TransportError::Internal);
        }
        let endpoints = views
            .into_iter()
            .map(|view| EndpointRow::from_view(view, minor))
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
    /// Whether `enabled` and `default` survive a daemon restart: `true`
    /// on 2.5 and later for a runtime keeping the endpoint overlay
    /// (ADR-0028 A 2026-10-11); `false` below 2.5, the 2.0 row.
    pub persisted: bool,
    /// Present while a connection holds the lease.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "absent_or_lease"
    )]
    pub lease: Option<LeaseRow>,
}

impl EndpointRow {
    /// `view` as the row `minor` names ([`EndpointList::from_views`]).
    ///
    /// # Errors
    /// [`TransportError::Internal`] for a lease outside its bounds.
    pub fn from_view(view: EndpointAdminView, minor: u64) -> Result<Self, TransportError> {
        Ok(Self {
            id: view.endpoint,
            enabled: view.enabled,
            default: view.default,
            persisted: minor >= ENDPOINT_PERSISTED_SINCE_MINOR && view.persisted,
            lease: view.lease.map(LeaseRow::try_from).transpose()?,
        })
    }
}

impl From<EndpointRow> for EndpointAdminView {
    /// A row as a client reads it back. The lease's `session_id` is
    /// binding-local and never on the wire, so it is `None` here
    /// (`LOCAL-CLIENT.md`, A 2026-09-30).
    fn from(row: EndpointRow) -> Self {
        let endpoint = row.id;
        Self {
            lease: row.lease.map(|lease| LeaseRecord {
                endpoint: endpoint.clone(),
                epoch: lease.epoch,
                client_kind: lease.client_kind,
                session_id: None,
            }),
            endpoint,
            enabled: row.enabled,
            default: row.default,
            persisted: row.persisted,
        }
    }
}

/// `ipc/trust-list` (2.1): one page of the data-plane allowlist, in the
/// ascending order of each peer's canonical string.
///
/// There is no default and no per-peer decision here: every peer not
/// listed is denied, which is the policy's shape rather than a setting
/// (ADR-0032).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TrustList {
    /// This profile's identity, on the FIRST page only (absent on every
    /// later one, and when the policy has none bound).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local_peer: Option<TransportIdentity>,
    /// At most [`MAX_TRUST_PAGE_ROWS`], strictly ascending by peer.
    pub allowed: Vec<TrustRow>,
    /// The last row's peer, when more remain: the next page's `after`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next: Option<TransportIdentity>,
}

impl TrustList {
    /// The page of `view` that follows `after` (the first when `None`),
    /// in the row shape the connection's `minor` negotiated: the 2.1 row
    /// below [`TRUST_SOURCE_SINCE_MINOR`], byte for byte as before it, and
    /// from it the 2.3 row a persisted view row becomes.
    ///
    /// The cursor is a POSITION, not a row: `after` need not be listed,
    /// so a peer revoked between two reads still names the page after
    /// it. Read against the live policy, so pages are not a snapshot.
    /// `a_trust_list_pages_in_order_and_says_when_more_remain`,
    /// `a_trust_row_is_the_shape_its_minor_names`.
    #[must_use]
    pub fn page(view: TrustAdminView, after: Option<&TransportIdentity>, minor: u64) -> Self {
        let mut rows = view.allowed;
        rows.sort_by(|a, b| a.peer.cmp(&b.peer));
        rows.dedup_by(|a, b| a.peer == b.peer);
        let mut rest = rows
            .into_iter()
            .filter(|row| after.is_none_or(|after| row.peer > *after))
            .peekable();
        let allowed: Vec<TrustRow> = rest
            .by_ref()
            .take(MAX_TRUST_PAGE_ROWS)
            .map(|row| {
                let persisted = minor >= TRUST_SOURCE_SINCE_MINOR && row.persisted;
                TrustRow {
                    peer: row.peer,
                    persisted,
                    source: persisted.then_some(row.source),
                }
            })
            .collect();
        let next = rest
            .peek()
            .is_some()
            .then(|| allowed.last().map(|row| row.peer.clone()))
            .flatten();
        Self {
            local_peer: if after.is_none() {
                view.local_peer
            } else {
                None
            },
            allowed,
            next,
        }
    }
}

impl<'de> Deserialize<'de> for TrustList {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            #[serde(default, deserialize_with = "absent_or_identity")]
            local_peer: Option<TransportIdentity>,
            allowed: Vec<TrustRow>,
            #[serde(default, deserialize_with = "absent_or_identity")]
            next: Option<TransportIdentity>,
        }
        let wire = Wire::deserialize(d)?;
        if wire.allowed.len() > MAX_TRUST_PAGE_ROWS {
            return Err(serde::de::Error::custom(format!(
                "at most {MAX_TRUST_PAGE_ROWS} rows, got {}",
                wire.allowed.len()
            )));
        }
        // Strictly ascending, which is unique too: the order is what makes
        // `next` a cursor a reader can trust.
        if wire
            .allowed
            .windows(2)
            .any(|pair| pair[0].peer >= pair[1].peer)
        {
            return Err(serde::de::Error::custom(
                "rows must be unique and in ascending peer order",
            ));
        }
        Ok(Self {
            local_peer: wire.local_peer,
            allowed: wire.allowed,
            next: wire.next,
        })
    }
}

/// `ipc/peer-list` (2.2): one page of the dial gate's state per
/// allowlisted peer, in the ascending order of each peer's canonical
/// string (`CONNECTIVITY.md` §19). Never an address.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PeerList {
    /// At most [`MAX_PEER_PAGE_ROWS`], strictly ascending by peer.
    pub peers: Vec<PeerRow>,
    /// The last row's peer, when more remain: the next page's `after`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next: Option<TransportIdentity>,
}

impl PeerList {
    /// The page of `rows` that follows `after` (the first when `None`):
    /// sorted and deduplicated by peer here, whatever order the port gave,
    /// so the cursor is a position as [`TrustList::page`]'s is.
    /// `a_peer_list_pages_in_order_and_says_when_more_remain`.
    #[must_use]
    pub fn page(mut rows: Vec<PeerGateView>, after: Option<&TransportIdentity>) -> Self {
        rows.sort_by(|a, b| a.peer.cmp(&b.peer));
        rows.dedup_by(|a, b| a.peer == b.peer);
        let mut rest = rows
            .into_iter()
            .filter(|row| after.is_none_or(|after| &row.peer > after))
            .peekable();
        let peers: Vec<PeerRow> = rest
            .by_ref()
            .take(MAX_PEER_PAGE_ROWS)
            .map(PeerRow::from)
            .collect();
        let next = rest
            .peek()
            .is_some()
            .then(|| peers.last().map(|row| row.peer.clone()))
            .flatten();
        Self { peers, next }
    }
}

impl<'de> Deserialize<'de> for PeerList {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            peers: Vec<PeerRow>,
            #[serde(default, deserialize_with = "absent_or_identity")]
            next: Option<TransportIdentity>,
        }
        let wire = Wire::deserialize(d)?;
        if wire.peers.len() > MAX_PEER_PAGE_ROWS {
            return Err(serde::de::Error::custom(format!(
                "at most {MAX_PEER_PAGE_ROWS} rows, got {}",
                wire.peers.len()
            )));
        }
        if wire
            .peers
            .windows(2)
            .any(|pair| pair[0].peer >= pair[1].peer)
        {
            return Err(serde::de::Error::custom(
                "rows must be unique and in ascending peer order",
            ));
        }
        Ok(Self {
            peers: wire.peers,
            next: wire.next,
        })
    }
}

/// One `peer-list` row: what the dial gate holds against an allowlisted
/// peer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PeerRow {
    /// The peer.
    pub peer: TransportIdentity,
    /// Whether any connection to it is open.
    pub connected: bool,
    /// Dials to it are refused until then, milliseconds since the Unix
    /// epoch.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "absent_or_millis"
    )]
    pub backoff_until: Option<u64>,
    /// Every known address of it is quarantined until then, the earliest
    /// release; absent while any is dialable (`CONNECTIVITY.md` §19).
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "absent_or_millis"
    )]
    pub quarantined_until: Option<u64>,
    /// How the last dial or connection ended; absent until the first.
    #[serde(default, skip_serializing_if = "Option::is_none", with = "outcome")]
    pub last_outcome: Option<PeerOutcome>,
}

impl From<PeerGateView> for PeerRow {
    fn from(view: PeerGateView) -> Self {
        Self {
            peer: view.peer,
            connected: view.connected,
            backoff_until: view.backoff_until,
            quarantined_until: view.quarantined_until,
            last_outcome: view.last_outcome,
        }
    }
}

fn absent_or_millis<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<u64>, D::Error> {
    u64::deserialize(d).map(Some)
}

/// `last_outcome` on the wire: the neutral `PeerOutcome` by the names
/// `CONNECTIVITY.md` §19 gives it; an explicit `null` is refused, as the
/// schema does.
mod outcome {
    use interweave_local_client_api::PeerOutcome;
    use serde::{Deserialize, Deserializer, Serializer};

    const ALL: [PeerOutcome; 4] = [
        PeerOutcome::Connected,
        PeerOutcome::DialFailed,
        PeerOutcome::IdentityMismatch,
        PeerOutcome::Denied,
    ];

    #[expect(
        clippy::ref_option,
        clippy::trivially_copy_pass_by_ref,
        reason = "serde's `with` hands the field by reference, whatever its size"
    )]
    pub(super) fn serialize<S: Serializer>(
        value: &Option<PeerOutcome>,
        s: S,
    ) -> Result<S::Ok, S::Error> {
        match value {
            Some(outcome) => s.serialize_str(outcome.as_str()),
            None => s.serialize_none(),
        }
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(
        d: D,
    ) -> Result<Option<PeerOutcome>, D::Error> {
        let name = String::deserialize(d)?;
        ALL.into_iter()
            .find(|o| o.as_str() == name)
            .map(Some)
            .ok_or_else(|| serde::de::Error::custom(format!("unknown last_outcome {name:?}")))
    }
}

/// One `trust-list` row: an allowed peer. The 2.1 row is `persisted`
/// false with no `source`; the 2.3 row `persisted` true with its
/// `source` -- the schema's if/then/else, held here in both directions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TrustRow {
    /// The allowed peer.
    pub peer: TransportIdentity,
    /// The row survives a restart (2.3): configured rows by
    /// `config.yaml`, administered rows by the trust overlay (ADR-0028 A
    /// 2026-10-07). Always false in the 2.1 row.
    pub persisted: bool,
    /// Where the row comes from, present exactly when `persisted` is.
    #[serde(skip_serializing_if = "Option::is_none", with = "source")]
    pub source: Option<TrustSource>,
}

impl<'de> Deserialize<'de> for TrustRow {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            peer: TransportIdentity,
            persisted: bool,
            #[serde(default, with = "source")]
            source: Option<TrustSource>,
        }
        let wire = Wire::deserialize(d)?;
        if wire.persisted != wire.source.is_some() {
            return Err(serde::de::Error::custom(
                "a persisted row names its source, and only a persisted row does",
            ));
        }
        Ok(Self {
            peer: wire.peer,
            persisted: wire.persisted,
            source: wire.source,
        })
    }
}

/// `source` on the wire: the neutral `TrustSource` by the schema's names;
/// an explicit `null` is refused.
mod source {
    use interweave_local_client_api::TrustSource;
    use serde::{Deserialize, Deserializer, Serializer};

    const ALL: [(TrustSource, &str); 2] = [
        (TrustSource::Configured, "configured"),
        (TrustSource::Administered, "administered"),
    ];

    #[expect(
        clippy::ref_option,
        clippy::trivially_copy_pass_by_ref,
        reason = "serde's `with` hands the field by reference, whatever its size"
    )]
    pub(super) fn serialize<S: Serializer>(
        value: &Option<TrustSource>,
        s: S,
    ) -> Result<S::Ok, S::Error> {
        match value.and_then(|v| ALL.into_iter().find(|(s, _)| *s == v)) {
            Some((_, name)) => s.serialize_str(name),
            None => s.serialize_none(),
        }
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(
        d: D,
    ) -> Result<Option<TrustSource>, D::Error> {
        let name = String::deserialize(d)?;
        ALL.into_iter()
            .find(|(_, n)| *n == name)
            .map(|(s, _)| Some(s))
            .ok_or_else(|| serde::de::Error::custom(format!("unknown source {name:?}")))
    }
}

fn absent_or_identity<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<Option<TransportIdentity>, D::Error> {
    TransportIdentity::deserialize(d).map(Some)
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

fn absent_or_ingress<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<Option<IngressCounters>, D::Error> {
    IngressCounters::deserialize(d).map(Some)
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
            persisted: false,
            lease: Some(LeaseRecord {
                endpoint: ep("human"),
                epoch: epoch(),
                client_kind: "human-client".into(),
                session_id: Some("s1".into()),
            }),
        };
        let list = EndpointList::from_views(vec![view], 4).expect("one row");
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

    /// A client reads back what the server built from the port, less the
    /// binding-local session id, which the wire never carries.
    /// The row is the shape the connection's minor names: below 2.5 the
    /// 2.0 row, `persisted: false` whatever the view says; from 2.5 the
    /// view's own, and a view that does not persist says so there too.
    /// Each reads back as the view the client is owed.
    #[test]
    fn an_endpoint_row_is_the_shape_its_minor_names() {
        let view = |persisted| EndpointAdminView {
            endpoint: ep("human"),
            enabled: false,
            default: false,
            persisted,
            lease: None,
        };
        for (persisted, minor, on_wire) in [
            (true, ENDPOINT_PERSISTED_SINCE_MINOR - 1, false),
            (true, ENDPOINT_PERSISTED_SINCE_MINOR, true),
            (false, ENDPOINT_PERSISTED_SINCE_MINOR, false),
            (false, 0, false),
        ] {
            let list = EndpointList::from_views(vec![view(persisted)], minor).expect("a row");
            assert_eq!(
                serde_json::to_value(&list).expect("ser"),
                json!({"endpoints": [{
                    "id": "human", "enabled": false, "default": false, "persisted": on_wire
                }]}),
                "persisted={persisted} minor={minor}"
            );
            let back = EndpointAdminView::from(list.endpoints[0].clone());
            assert_eq!(
                back.persisted, on_wire,
                "persisted={persisted} minor={minor}"
            );
        }
    }

    #[test]
    fn a_row_reads_back_as_the_view_without_its_session_id() {
        let lease = LeaseRecord {
            endpoint: ep("human"),
            epoch: epoch(),
            client_kind: "human-client".into(),
            session_id: Some("s1".into()),
        };
        let view = EndpointAdminView {
            endpoint: ep("human"),
            enabled: true,
            default: false,
            persisted: false,
            lease: Some(lease.clone()),
        };
        let row = EndpointRow::from_view(view.clone(), 4).expect("a row");
        let back = EndpointAdminView::from(row);
        assert_eq!(
            back,
            EndpointAdminView {
                lease: Some(LeaseRecord {
                    session_id: None,
                    ..lease
                }),
                ..view
            }
        );
    }

    #[test]
    fn a_directory_and_a_status_read_back_as_the_port_gave_them() {
        let directory = EndpointDirectoryV1 {
            generated_at_ms: 5,
            ttl_ms: 60_000,
            endpoints: vec![ep("human"), ep("bot")],
        };
        assert_eq!(
            EndpointDirectoryV1::from(DirectoryResult::from(directory.clone())),
            directory
        );
        let status = AdminStatus {
            health: Health::Degraded,
            peer: TransportIdentity::parse("12D3KooWDpJ7As7BWAwRMfu1VU2WCqNjvq387JEYKDBj4kx6nXTN")
                .expect("peer"),
            connectivity: interweave_transport_api::ConnectivitySummary {
                direct_inbound: interweave_transport_api::DirectInboundState::VerifiedPublic,
                relay_inbound: interweave_transport_api::PathReadiness::Ready,
                active_relay_reservations: 1,
                target_relay_reservations: 2,
                active_relayed_peer_paths: 3,
                hole_punch_inflight: 0,
                preferred_path_policy: interweave_transport_api::PreferredPathPolicy::DirectFirst,
                updated_at: 1_700_000_000_000,
            },
            active_leases: 3,
            pre_auth: Some(interweave_local_client_api::PreAuthCounts {
                tracked_sources: 4,
                pending: 2,
            }),
            ingress: Some(IngressCounts {
                direct_tracked_peers: 5,
                broadcast_tracked_peers: 6,
            }),
        };
        let result = AdminStatusResult::new(status.clone(), ServerCounters::default());
        let wire = serde_json::to_value(&result).expect("ser");
        assert_eq!(
            wire["pre_auth"],
            serde_json::json!({"tracked_sources": 4, "pending_total": 2}),
            "the pre-auth counts, and nothing peer-keyed"
        );
        assert_eq!(
            wire["ingress"],
            serde_json::json!({"direct_tracked_peers": 5, "broadcast_tracked_peers": 6}),
            "each lane under its own name"
        );
        assert_eq!(AdminStatus::from(result), status);
        // A block missing a lane is not the limiters' view, and reads as none.
        let mut half = wire.clone();
        half["ingress"] = serde_json::json!({"direct_tracked_peers": 5});
        let half: AdminStatusResult = serde_json::from_value(half).expect("de");
        assert_eq!(AdminStatus::from(half).ingress, None);
        // 1.1.0 dropped pre_auth.tracked_peers: a frame naming it is refused.
        let mut stale = wire.clone();
        stale["pre_auth"]["tracked_peers"] = serde_json::json!(1);
        assert!(serde_json::from_value::<AdminStatusResult>(stale).is_err());
        let none = AdminStatus {
            pre_auth: None,
            ingress: None,
            ..status
        };
        let wire = serde_json::to_value(AdminStatusResult::new(
            none.clone(),
            ServerCounters::default(),
        ))
        .expect("ser");
        assert!(
            wire.get("pre_auth").is_none() && wire.get("ingress").is_none(),
            "no funnel or limiter, no block: {wire}"
        );
        assert_eq!(
            AdminStatus::from(AdminStatusResult::new(
                none.clone(),
                ServerCounters::default()
            )),
            none
        );
    }

    /// The row this crate SENDS is bounded in characters too, so a session
    /// the data side admitted with a 64-character, 128-byte kind lists.
    #[test]
    fn a_lease_row_bounds_its_client_kind_in_characters() {
        let view = |kind: String| EndpointAdminView {
            endpoint: ep("human"),
            enabled: true,
            default: false,
            persisted: false,
            lease: Some(LeaseRecord {
                endpoint: ep("human"),
                epoch: epoch(),
                client_kind: kind,
                session_id: Some("s".into()),
            }),
        };
        assert!(EndpointList::from_views(vec![view("é".repeat(MAX_CLIENT_KIND_CHARS))], 4).is_ok());
        assert_eq!(
            EndpointList::from_views(vec![view("é".repeat(MAX_CLIENT_KIND_CHARS + 1))], 4),
            Err(TransportError::Internal)
        );
        // And the lower bound: an empty kind is not a label (`minLength: 1`).
        assert_eq!(
            EndpointList::from_views(vec![view(String::new())], 4),
            Err(TransportError::Internal)
        );
    }

    #[test]
    fn a_list_past_its_bound_or_repeating_an_id_is_refused_both_ways() {
        let view = |i: usize| EndpointAdminView {
            endpoint: ep(&format!("e{i}")),
            enabled: true,
            default: false,
            persisted: false,
            lease: None,
        };
        assert!(EndpointList::from_views((0..MAX_ENDPOINT_ROWS).map(view).collect(), 4).is_ok());
        assert_eq!(
            EndpointList::from_views((0..=MAX_ENDPOINT_ROWS).map(view).collect(), 4),
            Err(TransportError::Internal)
        );
        let row = json!({"id": "a", "enabled": true, "default": false, "persisted": false});
        assert!(
            serde_json::from_value::<EndpointList>(json!({"endpoints": [row.clone(), row]}))
                .is_err()
        );
        let not_a_bool = json!({"endpoints": [
            {"id": "a", "enabled": true, "default": false, "persisted": "yes"}
        ]});
        assert!(serde_json::from_value::<EndpointList>(not_a_bool).is_err());
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
            pre_auth: None,
            ingress: None,
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

    fn synthetic_peer(i: usize) -> TransportIdentity {
        let tail = format!("{i:044}").replace('0', "a");
        TransportIdentity::parse(format!("Qm{}", &tail[..44])).expect("peer")
    }

    fn gate_row(i: usize) -> PeerGateView {
        PeerGateView {
            peer: synthetic_peer(i),
            connected: i.is_multiple_of(2),
            backoff_until: i.is_multiple_of(3).then_some(1_791_329_950_227),
            quarantined_until: None,
            last_outcome: (!i.is_multiple_of(5)).then_some(PeerOutcome::Denied),
        }
    }

    /// The bound is what the body holds: a full page of the largest rows
    /// the schema admits -- every optional field, u64-maximum times, the
    /// longest outcome, an Ed25519 `PeerId`'s 52 characters (the longest a
    /// profile's key gives; a 46-character `Qm` id understates each row
    /// by six bytes) -- and a `next` serializes within `MAX_BODY_BYTES`
    /// with room for the response frame around it. Serializing does not
    /// ask the rows to differ, so one valid id fills every row.
    #[test]
    fn a_full_page_of_the_largest_rows_fits_the_body() {
        let longest = TransportIdentity::parse(
            "12D3KooWDpJ7As7BWAwRMfu1VU2WCqNjvq387JEYKDBj4kx6nXTN".to_owned(),
        )
        .expect("an Ed25519 PeerId");
        assert_eq!(longest.as_str().len(), 52);
        let page = PeerList {
            peers: (0..MAX_PEER_PAGE_ROWS)
                .map(|_| PeerRow {
                    peer: longest.clone(),
                    connected: false,
                    backoff_until: Some(u64::MAX),
                    quarantined_until: Some(u64::MAX),
                    last_outcome: Some(PeerOutcome::IdentityMismatch),
                })
                .collect(),
            next: Some(longest.clone()),
        };
        let bytes = serde_json::to_vec(&page).expect("serializes").len();
        assert!(
            bytes + 1_024 <= crate::framing::MAX_BODY_BYTES,
            "{bytes} bytes for {MAX_PEER_PAGE_ROWS} rows"
        );
        // And twice the bound does not: the reason the bound is 512.
        assert!(2 * bytes > crate::framing::MAX_BODY_BYTES);
    }

    #[test]
    fn a_peer_list_pages_in_order_and_says_when_more_remain() {
        // Two full pages and one row, offered out of order and with one
        // peer doubled.
        let mut rows: Vec<_> = (0..=2 * MAX_PEER_PAGE_ROWS).map(gate_row).collect();
        rows.push(gate_row(7));
        rows.reverse();
        let mut peers: Vec<_> = (0..=2 * MAX_PEER_PAGE_ROWS).map(synthetic_peer).collect();
        peers.sort();

        let mut after = None;
        let mut read = Vec::new();
        let mut pages = 0;
        loop {
            let page = PeerList::page(rows.clone(), after.as_ref());
            pages += 1;
            assert!(page.peers.len() <= MAX_PEER_PAGE_ROWS);
            read.extend(page.peers.iter().map(|row| row.peer.clone()));
            match page.next {
                Some(next) => {
                    assert_eq!(Some(&next), read.last(), "the last row is the cursor");
                    after = Some(next);
                }
                None => break,
            }
        }
        assert_eq!(pages, 3);
        assert_eq!(read, peers, "every peer once, ascending");

        // A row says what the view said, and the cursor is a position.
        let page = PeerList::page(vec![gate_row(3)], None);
        assert_eq!(page.peers, vec![PeerRow::from(gate_row(3))]);
        assert_eq!(page.next, None);
        let gone = peers[10].clone();
        let without: Vec<_> = rows.iter().filter(|r| r.peer != gone).cloned().collect();
        let page = PeerList::page(without, Some(&gone));
        assert_eq!(page.peers[0].peer, peers[11]);
    }

    #[test]
    fn a_peer_list_is_held_to_its_bound_its_order_and_its_names() {
        let row = |i: usize| json!({"peer": synthetic_peer(i).as_str(), "connected": true});
        let sorted = |n: usize| {
            let mut rows: Vec<_> = (0..n).map(row).collect();
            rows.sort_by(|a, b| a["peer"].as_str().cmp(&b["peer"].as_str()));
            rows
        };
        assert!(
            serde_json::from_value::<PeerList>(json!({"peers": sorted(MAX_PEER_PAGE_ROWS)}))
                .is_ok()
        );
        assert!(
            serde_json::from_value::<PeerList>(json!({"peers": sorted(MAX_PEER_PAGE_ROWS + 1)}))
                .is_err()
        );
        let mut unordered = sorted(3);
        unordered.swap(0, 2);
        assert!(serde_json::from_value::<PeerList>(json!({"peers": unordered})).is_err());
        let doubled = vec![row(1), row(1)];
        assert!(serde_json::from_value::<PeerList>(json!({"peers": doubled})).is_err());
        assert!(serde_json::from_value::<PeerList>(json!({"peers": [], "next": null})).is_err());
        // Every outcome by its section 19 name, both ways, and nothing else.
        for outcome in [
            PeerOutcome::Connected,
            PeerOutcome::DialFailed,
            PeerOutcome::IdentityMismatch,
            PeerOutcome::Denied,
        ] {
            let mut r = row(1);
            r["last_outcome"] = json!(outcome.as_str());
            let page: PeerList =
                serde_json::from_value(json!({"peers": [r.clone()]})).expect("valid");
            assert_eq!(page.peers[0].last_outcome, Some(outcome));
            assert_eq!(
                serde_json::to_value(&page).expect("serializes")["peers"][0],
                r
            );
        }
        for bad in [json!("refused"), json!("timeout"), json!(null), json!(3)] {
            let mut r = row(1);
            r["last_outcome"] = bad;
            assert!(serde_json::from_value::<PeerList>(json!({"peers": [r]})).is_err());
        }
        let mut r = row(1);
        r["backoff_until"] = json!(null);
        assert!(
            serde_json::from_value::<PeerList>(json!({"peers": [r]})).is_err(),
            "a null time"
        );
        let mut r = row(1);
        r["address"] = json!("/ip4/10.0.0.1/tcp/1");
        assert!(
            serde_json::from_value::<PeerList>(json!({"peers": [r]})).is_err(),
            "no address field"
        );
    }

    #[test]
    fn a_trust_list_pages_in_order_and_says_when_more_remain() {
        let local = synthetic_peer(9_999);
        // Two full pages and one row, offered out of order.
        let mut peers: Vec<_> = (0..=2 * MAX_TRUST_PAGE_ROWS).map(synthetic_peer).collect();
        peers.reverse();
        let view = TrustAdminView {
            local_peer: Some(local.clone()),
            allowed: peers
                .clone()
                .into_iter()
                .map(|peer| interweave_local_client_api::TrustedPeer {
                    peer,
                    persisted: false,
                    source: interweave_local_client_api::TrustSource::Configured,
                })
                .collect(),
        };
        peers.sort();

        let mut after = None;
        let mut read = Vec::new();
        let mut pages = 0;
        loop {
            let page = TrustList::page(view.clone(), after.as_ref(), crate::IPC_MAX_MINOR);
            pages += 1;
            assert_eq!(
                page.local_peer.as_ref(),
                (pages == 1).then_some(&local),
                "the local peer on the first page only"
            );
            assert!(page.allowed.len() <= MAX_TRUST_PAGE_ROWS);
            read.extend(page.allowed.iter().map(|row| row.peer.clone()));
            match page.next {
                Some(next) => {
                    assert_eq!(Some(&next), read.last(), "the last row is the cursor");
                    after = Some(next);
                }
                None => break,
            }
        }
        assert_eq!(pages, 3);
        assert_eq!(read, peers, "every peer once, ascending");

        // Exactly one page: no `next`, so the reader stops.
        let full = TrustAdminView {
            local_peer: None,
            allowed: peers[..MAX_TRUST_PAGE_ROWS]
                .iter()
                .cloned()
                .map(|peer| interweave_local_client_api::TrustedPeer {
                    peer,
                    persisted: false,
                    source: interweave_local_client_api::TrustSource::Configured,
                })
                .collect(),
        };
        let page = TrustList::page(full, None, crate::IPC_MAX_MINOR);
        assert_eq!(page.allowed.len(), MAX_TRUST_PAGE_ROWS);
        assert_eq!(page.next, None);
        assert_eq!(page.local_peer, None, "absent when the policy has none");

        // A cursor naming a peer no longer listed is a position.
        let gone = peers[10].clone();
        let mut without = view.clone();
        without.allowed.retain(|row| row.peer != gone);
        let page = TrustList::page(without, Some(&gone), crate::IPC_MAX_MINOR);
        assert_eq!(page.allowed[0].peer, peers[11]);
    }

    #[test]
    fn a_trust_list_is_held_to_its_bound_and_its_order() {
        let row = |i| json!({"peer": synthetic_peer(i).as_str(), "persisted": false});
        let rows = |n: usize| (0..n).map(row).collect::<Vec<_>>();
        let mut sorted = rows(MAX_TRUST_PAGE_ROWS);
        sorted.sort_by(|a, b| a["peer"].as_str().cmp(&b["peer"].as_str()));
        assert!(serde_json::from_value::<TrustList>(json!({"allowed": sorted})).is_ok());
        let mut over = rows(MAX_TRUST_PAGE_ROWS + 1);
        over.sort_by(|a, b| a["peer"].as_str().cmp(&b["peer"].as_str()));
        assert!(serde_json::from_value::<TrustList>(json!({"allowed": over})).is_err());
        let (a, b) = (row(1), row(2));
        let (low, high) = if a["peer"].as_str() < b["peer"].as_str() {
            (a, b)
        } else {
            (b, a)
        };
        assert!(
            serde_json::from_value::<TrustList>(json!({"allowed": [low.clone(), high.clone()]}))
                .is_ok()
        );
        assert!(
            serde_json::from_value::<TrustList>(json!({"allowed": [high, low.clone()]})).is_err(),
            "out of order"
        );
        assert!(
            serde_json::from_value::<TrustList>(json!({"allowed": [low.clone(), low]})).is_err(),
            "a duplicate"
        );
        assert!(
            serde_json::from_value::<TrustList>(json!({"allowed": [], "next": null})).is_err(),
            "next is a peer or absent"
        );
        // The schema's if/then/else: a persisted row names its source,
        // and only a persisted row does.
        let one = |row: serde_json::Value| {
            serde_json::from_value::<TrustList>(json!({ "allowed": [row] })).map(|_| ())
        };
        let p = synthetic_peer(0);
        assert!(
            one(json!({"peer": p.as_str(), "persisted": true, "source": "configured"})).is_ok()
        );
        assert!(
            one(json!({"peer": p.as_str(), "persisted": true, "source": "administered"})).is_ok()
        );
        assert!(one(json!({"peer": p.as_str(), "persisted": false})).is_ok());
        assert!(
            one(json!({"peer": p.as_str(), "persisted": true})).is_err(),
            "persisted without its source"
        );
        assert!(
            one(json!({"peer": p.as_str(), "persisted": false, "source": "configured"})).is_err(),
            "a source on the 2.1 row"
        );
        assert!(
            one(json!({"peer": p.as_str(), "persisted": true, "source": null})).is_err(),
            "an explicit null"
        );
        assert!(
            one(json!({"peer": p.as_str(), "persisted": true, "source": "someone"})).is_err(),
            "an unknown source"
        );
    }

    /// Below 2.3 the row is the 2.1 row byte for byte, whatever the
    /// binding knows; from 2.3 a persisted row carries its source. The
    /// page renders a row the binding does not persist as the 2.1 row at
    /// every minor; at 2.3 the server refuses to serve one (ipc-server's
    /// `an_unpersisted_trust_row_is_refused_at_two_three`).
    #[test]
    fn a_trust_row_is_the_shape_its_minor_names() {
        let (configured, administered) = (synthetic_peer(1), synthetic_peer(2));
        let view = |persisted| TrustAdminView {
            local_peer: None,
            allowed: vec![
                interweave_local_client_api::TrustedPeer {
                    peer: configured.clone(),
                    persisted,
                    source: TrustSource::Configured,
                },
                interweave_local_client_api::TrustedPeer {
                    peer: administered.clone(),
                    persisted,
                    source: TrustSource::Administered,
                },
            ],
        };
        let rows = |persisted, minor| {
            let page = serde_json::to_value(TrustList::page(view(persisted), None, minor))
                .expect("serializes");
            let mut rows = page["allowed"].as_array().expect("rows").clone();
            rows.sort_by(|a, b| a["peer"].as_str().cmp(&b["peer"].as_str()));
            rows
        };
        let old = |p: &TransportIdentity| json!({"peer": p.as_str(), "persisted": false});
        for minor in 0..TRUST_SOURCE_SINCE_MINOR {
            assert_eq!(
                rows(true, minor),
                [old(&configured), old(&administered)],
                "2.{minor}"
            );
        }
        assert_eq!(
            rows(true, TRUST_SOURCE_SINCE_MINOR),
            [
                json!({"peer": configured.as_str(), "persisted": true, "source": "configured"}),
                json!({"peer": administered.as_str(), "persisted": true, "source": "administered"}),
            ]
        );
        assert_eq!(
            rows(false, TRUST_SOURCE_SINCE_MINOR),
            [old(&configured), old(&administered)],
            "the page renders an unpersisted row as the 2.1 row; the server refuses it at 2.3"
        );
    }

    /// A full page of the largest 2.3 rows -- 52-character Ed25519 ids,
    /// `administered`, the first page's `local_peer` and a `next` -- fits
    /// the body with room for the frame.
    #[test]
    fn a_full_trust_page_of_the_largest_rows_fits_the_body() {
        let longest = TransportIdentity::parse(
            "12D3KooWDpJ7As7BWAwRMfu1VU2WCqNjvq387JEYKDBj4kx6nXTN".to_owned(),
        )
        .expect("an Ed25519 PeerId");
        let page = TrustList {
            local_peer: Some(longest.clone()),
            allowed: (0..MAX_TRUST_PAGE_ROWS)
                .map(|_| TrustRow {
                    peer: longest.clone(),
                    persisted: true,
                    source: Some(TrustSource::Administered),
                })
                .collect(),
            next: Some(longest),
        };
        let bytes = serde_json::to_vec(&page).expect("serializes").len();
        assert!(
            bytes + 1_024 <= crate::framing::MAX_BODY_BYTES,
            "{bytes} bytes for {MAX_TRUST_PAGE_ROWS} rows"
        );
    }
}
