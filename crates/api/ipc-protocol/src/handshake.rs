// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The IPC v2 hello exchange.
//!
//! The handshake is where authority is decided, so the types here keep
//! two facts apart that a single "capabilities" field would blur:
//! **requested** and **granted**. A client asks in [`Hello`]; the server
//! answers in [`HelloResponse`] with what policy actually allowed. Nothing
//! copies one into the other.
//!
//! # The socket is an input the frame cannot influence
//!
//! Which socket a client connected to decides its authority domain, and
//! no field in [`Hello`] can change it (ADR-0037). That is modelled as
//! [`AuthorityDomain`], supplied by the accepting code rather than parsed
//! from the frame — a client claiming `client.kind = "admin"` on the data
//! socket is still on the data socket, and [`Hello::evaluate`] answers
//! accordingly.

use std::collections::BTreeSet;

use interweave_local_client_api::{AdminCapability, DataCapability, MAX_CLIENT_KIND_BYTES};
use interweave_transport_api::{EndpointId, TransportError};
use serde::{Deserialize, Serialize};

/// The IPC major version this crate implements.
pub const IPC_MAJOR: u32 = 2;
/// Maximum requested capabilities or negotiated features.
pub const MAX_REQUESTED: usize = 8;

/// Maximum bytes in one negotiated feature name.
///
/// `ipc/hello.schema.json` states `minLength: 1, maxLength: 64` on each
/// feature. The lower bound matters as much as the upper: an empty
/// feature name is a request for nothing that still consumes a slot.
pub const MAX_FEATURE_BYTES: usize = 64;

/// Maximum bytes in the optional client version string.
pub const MAX_CLIENT_VERSION_BYTES: usize = 128;
/// The feature name a client negotiates for keepalive.
pub const FEATURE_KEEPALIVE: &str = "keepalive";

/// Which socket the connection arrived on.
///
/// Supplied by the accepting code from the listener it came from, never
/// parsed out of the frame. This is the whole of ADR-0037's privilege
/// separation: the frame is data, the socket is authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthorityDomain {
    /// The data-plane socket. Can never yield `admin.*`.
    Data,
    /// The administrative socket. Never holds an endpoint lease.
    Admin,
}

/// The negotiated version pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IpcVersion {
    /// Always 2 for this contract.
    pub major: u32,
    /// Minor version, negotiated.
    pub minor: u32,
}

/// The client's self-description.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClientInfo {
    /// A hygiene label, never authentication and never an authority selector.
    pub kind: String,
    /// Optional client version string.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

/// The endpoint a client is claiming.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EndpointClaim {
    /// The configured endpoint requested.
    pub id: EndpointId,
}

/// A client's first frame.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Hello {
    /// Always `"hello"`.
    #[serde(rename = "type")]
    pub frame_type: HelloTag,
    /// The version being proposed.
    pub ipc_version: IpcVersion,
    /// Who is connecting.
    pub client: ClientInfo,
    /// The endpoint claim, absent for diagnostics and required absent on admin.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<EndpointClaim>,
    /// Capabilities **requested**, not granted.
    #[serde(
        default,
        skip_serializing_if = "BTreeSet::is_empty",
        deserialize_with = "wire_set"
    )]
    pub requested_capabilities: BTreeSet<RequestedCapability>,
    /// Optional negotiated features, e.g. `keepalive`.
    #[serde(
        default,
        skip_serializing_if = "BTreeSet::is_empty",
        deserialize_with = "wire_feature_set"
    )]
    pub features: BTreeSet<String>,
}

/// The literal `"hello"` discriminant, as its own type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HelloTag {
    /// The only legal value.
    #[serde(rename = "hello")]
    Hello,
}

/// A capability a client may ASK for.
///
/// This one *can* name `admin.*`, unlike
/// [`interweave_local_client_api::DataCapability`] — a client is allowed
/// to ask for anything, and refusing the request is the server's job. The
/// two types being different is what makes "asked" and "holds"
/// unconfusable: a request never becomes a grant by assignment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum RequestedCapability {
    /// Receive events.
    #[serde(rename = "events")]
    Events,
    /// Issue commands.
    #[serde(rename = "commands")]
    Commands,
    /// Query the endpoint directory.
    #[serde(rename = "endpoints.query")]
    EndpointsQuery,
    /// Administer endpoints.
    #[serde(rename = "admin.endpoints")]
    AdminEndpoints,
    /// Shut down the runtime.
    #[serde(rename = "admin.shutdown")]
    AdminShutdown,
}

impl RequestedCapability {
    /// The data capability this request maps to, if it is one.
    #[must_use]
    pub const fn as_data(self) -> Option<DataCapability> {
        match self {
            Self::Events => Some(DataCapability::Events),
            Self::Commands => Some(DataCapability::Commands),
            Self::EndpointsQuery => Some(DataCapability::EndpointsQuery),
            Self::AdminEndpoints | Self::AdminShutdown => None,
        }
    }

    /// The admin capability this request maps to, if it is one.
    #[must_use]
    pub const fn as_admin(self) -> Option<AdminCapability> {
        match self {
            Self::AdminEndpoints => Some(AdminCapability::Endpoints),
            Self::AdminShutdown => Some(AdminCapability::Shutdown),
            Self::Events | Self::Commands | Self::EndpointsQuery => None,
        }
    }
}

/// What the handshake decided, before any lease is attempted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandshakeOutcome {
    /// Data capabilities this connection may hold.
    pub granted_data: BTreeSet<DataCapability>,
    /// Admin capabilities this connection may hold.
    pub granted_admin: BTreeSet<AdminCapability>,
    /// The endpoint to attempt a lease for, if any.
    pub endpoint: Option<EndpointId>,
}

/// Deserialize a wire array into a set, enforcing the WIRE cardinality
/// first.
///
/// # Why this cannot be `BTreeSet` directly
///
/// Collecting into a set during deserialization destroys the evidence
/// the contract is about. `ipc/hello.schema.json` declares `uniqueItems:
/// true` and `maxItems: 8`; nine copies of one capability violate both,
/// and both violations vanish the instant serde inserts them into a set —
/// `evaluate` then counts one member and admits a frame every conforming
/// validator refuses.
///
/// So the sequence is read as a `Vec`, judged as it arrived, and only
/// then collected.
fn wire_set<'de, D, T>(deserializer: D) -> Result<BTreeSet<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::Deserialize<'de> + Ord,
{
    use serde::de::Error as _;
    let items = Vec::<T>::deserialize(deserializer)?;
    if items.len() > MAX_REQUESTED {
        return Err(D::Error::custom(format!(
            "at most {MAX_REQUESTED} entries, got {}",
            items.len()
        )));
    }
    let count = items.len();
    let set: BTreeSet<T> = items.into_iter().collect();
    // Compared after collecting: a shorter set means the wire array
    // repeated something, which `uniqueItems: true` forbids. This is the
    // whole point of the detour through `Vec` — after the collect, the
    // duplicate is unrecoverable.
    if set.len() != count {
        return Err(D::Error::custom(
            "requested_capabilities must be unique on the wire",
        ));
    }
    Ok(set)
}

/// The same, plus the per-name length bounds the schema states.
fn wire_feature_set<'de, D>(deserializer: D) -> Result<BTreeSet<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error as _;
    let items = Vec::<String>::deserialize(deserializer)?;
    if items.len() > MAX_REQUESTED {
        return Err(D::Error::custom(format!(
            "at most {MAX_REQUESTED} features, got {}",
            items.len()
        )));
    }
    for name in &items {
        if name.is_empty() || name.len() > MAX_FEATURE_BYTES {
            return Err(D::Error::custom(format!(
                "feature names are 1..={MAX_FEATURE_BYTES} bytes, got {}",
                name.len()
            )));
        }
    }
    let count = items.len();
    let set: BTreeSet<String> = items.into_iter().collect();
    if set.len() != count {
        return Err(D::Error::custom("features must be unique"));
    }
    Ok(set)
}

impl Hello {
    /// Decide what this hello may be granted in the given authority domain.
    ///
    /// Policy still narrows the result afterwards — this answers only what
    /// the domain and the frame make *possible*. Refusals here are the
    /// categorical ones, and each maps to the error the contract names.
    ///
    /// # Errors
    /// - [`TransportError::VersionIncompatible`] for a major other than 2;
    /// - [`TransportError::InvalidArgument`] for an out-of-range client
    ///   kind or over-cap request lists;
    /// - [`TransportError::CapabilityDenied`] for `admin.*` requested on
    ///   the data socket, an endpoint claimed on the admin socket, or an
    ///   endpoint claimed without negotiating keepalive when the profile
    ///   requires it.
    pub fn evaluate(
        &self,
        domain: AuthorityDomain,
        keepalive_required_for_lease: bool,
    ) -> Result<HandshakeOutcome, TransportError> {
        if self.ipc_version.major != IPC_MAJOR {
            return Err(TransportError::VersionIncompatible);
        }
        if self.client.kind.is_empty() || self.client.kind.len() > MAX_CLIENT_KIND_BYTES {
            return Err(TransportError::InvalidArgument);
        }
        if self.requested_capabilities.len() > MAX_REQUESTED || self.features.len() > MAX_REQUESTED
        {
            return Err(TransportError::InvalidArgument);
        }

        let wants_admin = self
            .requested_capabilities
            .iter()
            .any(|c| c.as_admin().is_some());

        let wants_data = self
            .requested_capabilities
            .iter()
            .any(|c| c.as_data().is_some());

        match domain {
            AuthorityDomain::Data => {
                // The categorical rule. Not "unlikely", not "policy will
                // probably refuse": a data connection is ineligible for
                // admin.* regardless of what it claims to be.
                if wants_admin {
                    return Err(TransportError::CapabilityDenied);
                }
                // `endpoint` may be omitted ONLY by a read-only diagnostics
                // client that does not need direct send/receive
                // (LOCAL-IPC.md). `commands` is exactly the capability such
                // a client does not need, so the pair is contradictory —
                // and granting it would create a command-capable session
                // with no source endpoint, which is the state ADR-0030's
                // non-spoofable source exists to make impossible.
                if self.endpoint.is_none()
                    && self
                        .requested_capabilities
                        .contains(&RequestedCapability::Commands)
                {
                    return Err(TransportError::CapabilityDenied);
                }
                if self.endpoint.is_some()
                    && keepalive_required_for_lease
                    && !self.features.iter().any(|f| f == FEATURE_KEEPALIVE)
                {
                    // Denied at claim time rather than granted and then
                    // revoked: a lease that exists for one round trip is
                    // a lease another client could not take.
                    return Err(TransportError::CapabilityDenied);
                }
                Ok(HandshakeOutcome {
                    granted_data: self
                        .requested_capabilities
                        .iter()
                        .filter_map(|c| c.as_data())
                        .collect(),
                    granted_admin: BTreeSet::new(),
                    endpoint: self.endpoint.as_ref().map(|e| e.id.clone()),
                })
            }
            AuthorityDomain::Admin => {
                // An admin connection never owns an application endpoint.
                if self.endpoint.is_some() {
                    return Err(TransportError::CapabilityDenied);
                }
                // And it never gets application messaging: LOCAL-IPC.md
                // says the admin socket does not grant `events`/`commands`.
                // REFUSED rather than filtered out, mirroring the data-side
                // rule — silently dropping them would let an admin client
                // believe it holds a data grant it does not, and leave the
                // boundary depending on every later caller re-filtering.
                if wants_data {
                    return Err(TransportError::CapabilityDenied);
                }
                Ok(HandshakeOutcome {
                    granted_data: BTreeSet::new(),
                    granted_admin: self
                        .requested_capabilities
                        .iter()
                        .filter_map(|c| c.as_admin())
                        .collect(),
                    endpoint: None,
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hello(kind: &str, caps: &[RequestedCapability], endpoint: Option<&str>) -> Hello {
        Hello {
            frame_type: HelloTag::Hello,
            ipc_version: IpcVersion { major: 2, minor: 0 },
            client: ClientInfo {
                kind: kind.to_owned(),
                version: None,
            },
            endpoint: endpoint.map(|e| EndpointClaim {
                id: EndpointId::parse(e).expect("valid endpoint"),
            }),
            requested_capabilities: caps.iter().copied().collect(),
            features: [FEATURE_KEEPALIVE.to_owned()].into_iter().collect(),
        }
    }

    #[test]
    fn a_data_client_gets_the_data_capabilities_it_asked_for() {
        let h = hello(
            "human-client",
            &[RequestedCapability::Events, RequestedCapability::Commands],
            Some("human"),
        );
        let out = h.evaluate(AuthorityDomain::Data, true).expect("granted");
        assert!(out.granted_data.contains(&DataCapability::Events));
        assert!(out.granted_data.contains(&DataCapability::Commands));
        assert!(out.granted_admin.is_empty());
        assert_eq!(out.endpoint, Some(EndpointId::parse("human").expect("ok")));
    }

    #[test]
    fn admin_capabilities_are_categorically_refused_on_the_data_socket() {
        // Not filtered out silently — refused, so a client cannot believe
        // it received an authority it did not.
        let h = hello("tool", &[RequestedCapability::AdminShutdown], None);
        assert_eq!(
            h.evaluate(AuthorityDomain::Data, false),
            Err(TransportError::CapabilityDenied)
        );

        // Even mixed with legitimate requests.
        let h = hello(
            "tool",
            &[
                RequestedCapability::Events,
                RequestedCapability::AdminEndpoints,
            ],
            None,
        );
        assert_eq!(
            h.evaluate(AuthorityDomain::Data, false),
            Err(TransportError::CapabilityDenied)
        );
    }

    #[test]
    fn claiming_an_administrative_client_kind_changes_nothing() {
        // The socket decides the domain; the frame cannot influence it.
        let h = hello("admin", &[RequestedCapability::AdminShutdown], None);
        assert_eq!(
            h.evaluate(AuthorityDomain::Data, false),
            Err(TransportError::CapabilityDenied)
        );
        // The same frame on the admin socket is fine, because the socket
        // is what changed — not anything the client said.
        let out = h.evaluate(AuthorityDomain::Admin, false).expect("granted");
        assert!(out.granted_admin.contains(&AdminCapability::Shutdown));
    }

    #[test]
    fn an_admin_connection_gets_no_application_messaging() {
        // The mirror of the data-side rule, and refused rather than
        // filtered for the same reason: an admin client must not come away
        // believing it holds a data grant.
        let h = hello(
            "settings",
            &[
                RequestedCapability::AdminEndpoints,
                RequestedCapability::Events,
            ],
            None,
        );
        assert_eq!(
            h.evaluate(AuthorityDomain::Admin, false),
            Err(TransportError::CapabilityDenied)
        );

        // Admin-only requests are unaffected.
        let h = hello("settings", &[RequestedCapability::AdminEndpoints], None);
        let out = h.evaluate(AuthorityDomain::Admin, false).expect("granted");
        assert!(out.granted_data.is_empty());
        assert!(out.granted_admin.contains(&AdminCapability::Endpoints));
    }

    #[test]
    fn a_data_hello_wanting_commands_must_claim_an_endpoint() {
        // `endpoint` may be omitted only by a read-only diagnostics client
        // that does not need direct send/receive, and `commands` is
        // precisely what such a client does not need. Granting the pair
        // would build a command-capable session with no source endpoint.
        let h = hello("tool", &[RequestedCapability::Commands], None);
        assert_eq!(
            h.evaluate(AuthorityDomain::Data, false),
            Err(TransportError::CapabilityDenied)
        );

        // Read-only capabilities without an endpoint remain legal.
        let h = hello(
            "diagnostics",
            &[
                RequestedCapability::Events,
                RequestedCapability::EndpointsQuery,
            ],
            None,
        );
        assert!(h.evaluate(AuthorityDomain::Data, false).is_ok());

        // And with an endpoint, commands are fine.
        let h = hello(
            "human-client",
            &[RequestedCapability::Commands],
            Some("human"),
        );
        assert!(h.evaluate(AuthorityDomain::Data, true).is_ok());
    }

    #[test]
    fn an_admin_connection_may_not_claim_an_endpoint() {
        let h = hello(
            "settings",
            &[RequestedCapability::AdminEndpoints],
            Some("human"),
        );
        assert_eq!(
            h.evaluate(AuthorityDomain::Admin, false),
            Err(TransportError::CapabilityDenied)
        );
    }

    #[test]
    fn a_lease_claim_without_keepalive_is_denied_at_claim_time() {
        // Rather than granted and revoked a moment later: a lease that
        // exists for one round trip is a lease no other client could take.
        let mut h = hello(
            "human-client",
            &[RequestedCapability::Commands],
            Some("human"),
        );
        h.features.clear();
        assert_eq!(
            h.evaluate(AuthorityDomain::Data, true),
            Err(TransportError::CapabilityDenied)
        );
        // With the policy off, the same frame is fine.
        assert!(h.evaluate(AuthorityDomain::Data, false).is_ok());
        // And a connection claiming no endpoint never needed keepalive.
        let mut diagnostics = hello("diagnostics", &[RequestedCapability::Events], None);
        diagnostics.features.clear();
        assert!(diagnostics.evaluate(AuthorityDomain::Data, true).is_ok());
    }

    #[test]
    fn version_and_bounds_are_enforced() {
        let mut h = hello("k", &[], None);
        h.ipc_version.major = 1;
        assert_eq!(
            h.evaluate(AuthorityDomain::Data, false),
            Err(TransportError::VersionIncompatible)
        );

        let mut h = hello("k", &[], None);
        h.client.kind = String::new();
        assert_eq!(
            h.evaluate(AuthorityDomain::Data, false),
            Err(TransportError::InvalidArgument)
        );

        let mut h = hello("k", &[], None);
        h.client.kind = "k".repeat(65);
        assert_eq!(
            h.evaluate(AuthorityDomain::Data, false),
            Err(TransportError::InvalidArgument)
        );

        let mut h = hello("k", &[], None);
        h.features = (0..9).map(|i| format!("f{i}")).collect();
        assert_eq!(
            h.evaluate(AuthorityDomain::Data, false),
            Err(TransportError::InvalidArgument)
        );
    }

    #[test]
    fn a_request_is_not_a_grant() {
        // The two vocabularies are different types on purpose: nothing
        // can assign a requested capability into a granted set.
        assert_eq!(
            RequestedCapability::Events.as_data(),
            Some(DataCapability::Events)
        );
        assert_eq!(RequestedCapability::AdminShutdown.as_data(), None);
        assert_eq!(
            RequestedCapability::AdminShutdown.as_admin(),
            Some(AdminCapability::Shutdown)
        );
        assert_eq!(RequestedCapability::Events.as_admin(), None);
    }

    #[test]
    fn hello_rejects_unknown_fields_and_wrong_tags() {
        let json = serde_json::json!({
            "type": "hello",
            "ipc_version": { "major": 2, "minor": 0 },
            "client": { "kind": "x" },
            "surprise": true
        });
        assert!(serde_json::from_value::<Hello>(json).is_err());

        let json = serde_json::json!({
            "type": "goodbye",
            "ipc_version": { "major": 2, "minor": 0 },
            "client": { "kind": "x" }
        });
        assert!(serde_json::from_value::<Hello>(json).is_err());
    }
    #[test]
    fn wire_duplicates_are_refused_before_the_set_hides_them() {
        // Nine copies violate both `uniqueItems` and `maxItems: 8`, and
        // both violations vanish the instant serde collects into a set.
        // The check has to happen on the sequence or not at all.
        let nine = r#"{"type":"hello","ipc_version":{"major":2,"minor":0},
            "client":{"kind":"human-client"},
            "requested_capabilities":["events","events","events","events",
            "events","events","events","events","events"]}"#;
        assert!(serde_json::from_str::<Hello>(nine).is_err());

        let two = r#"{"type":"hello","ipc_version":{"major":2,"minor":0},
            "client":{"kind":"human-client"},
            "requested_capabilities":["events","events"]}"#;
        assert!(
            serde_json::from_str::<Hello>(two).is_err(),
            "a duplicate within the cap is still a duplicate"
        );

        let ok = r#"{"type":"hello","ipc_version":{"major":2,"minor":0},
            "client":{"kind":"human-client"},
            "requested_capabilities":["events","commands"]}"#;
        assert!(serde_json::from_str::<Hello>(ok).is_ok());
    }

    #[test]
    fn feature_names_outside_their_bounds_are_refused() {
        let with = |f: &str| {
            format!(
                r#"{{"type":"hello","ipc_version":{{"major":2,"minor":0}},
                "client":{{"kind":"human-client"}},"features":["{f}"]}}"#
            )
        };
        assert!(serde_json::from_str::<Hello>(&with("")).is_err(), "empty");
        assert!(
            serde_json::from_str::<Hello>(&with(&"x".repeat(MAX_FEATURE_BYTES + 1))).is_err(),
            "over maxLength"
        );
        assert!(serde_json::from_str::<Hello>(&with(&"x".repeat(MAX_FEATURE_BYTES))).is_ok());
    }
}
