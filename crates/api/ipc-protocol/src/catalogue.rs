// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The closed method catalogue (`ipc/method.schema.json`) and THE table
//! of method -> authority domain -> required capability -> since
//! (`LOCAL-IPC.md` §Method catalogue; plan §16 (3)).
//!
//! The capability a method needs is not in the schema -- the contract
//! meta-schema admits no such annotation -- so it lives in two places:
//! the prose table and [`Method::entry`]. `tests/schema_agreement.rs`
//! binds the two, and the names to the schema, in both directions.

use serde::{Deserialize, Serialize};

use crate::handshake::{AuthorityDomain, RequestedCapability};
use crate::version::IpcVersion;

/// One request method of the closed catalogue.
///
/// A name outside it is answered `ProtocolUnsupported` and the connection
/// stays, which is why a request's envelope carries the name as text and
/// [`Method::parse`] is asked afterwards rather than serde refusing the
/// frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Method {
    /// Take a join reference on a channel.
    #[serde(rename = "channel.join")]
    ChannelJoin,
    /// Release a join reference.
    #[serde(rename = "channel.leave")]
    ChannelLeave,
    /// Publish on a joined channel.
    #[serde(rename = "broadcast.publish")]
    BroadcastPublish,
    /// Send a directed message from the connection's leased endpoint.
    #[serde(rename = "direct.send")]
    DirectSend,
    /// Query a trusted peer's endpoint directory.
    #[serde(rename = "endpoints.query")]
    EndpointsQuery,
    /// Read the administrative status view.
    #[serde(rename = "admin.status")]
    AdminStatus,
    /// List the configured endpoints and their runtime state.
    #[serde(rename = "admin.endpoints.list")]
    AdminEndpointsList,
    /// Revoke an endpoint's live lease.
    #[serde(rename = "admin.endpoints.revoke")]
    AdminEndpointsRevoke,
    /// Enable or disable an endpoint (runtime overlay).
    #[serde(rename = "admin.endpoints.set_enabled")]
    AdminEndpointsSetEnabled,
    /// Set or clear the default endpoint (runtime overlay).
    #[serde(rename = "admin.endpoints.set_default")]
    AdminEndpointsSetDefault,
    /// Shut the runtime down.
    #[serde(rename = "admin.shutdown")]
    AdminShutdown,
    /// One page of the data-plane allowlist (2.1).
    #[serde(rename = "admin.trust.list")]
    AdminTrustList,
    /// Allow a peer or revoke it (2.1, runtime overlay).
    #[serde(rename = "admin.trust.set")]
    AdminTrustSet,
    /// One page of the dial gate's per-peer state (2.2, under
    /// `admin.status`).
    #[serde(rename = "admin.peers.list")]
    AdminPeersList,
}

/// One row of the catalogue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MethodEntry {
    /// The socket the method is served on; on the other it is refused
    /// `CapabilityDenied` before dispatch (ADR-0037).
    pub domain: AuthorityDomain,
    /// The capability the connection must hold.
    pub capability: RequestedCapability,
    /// The IPC minor (of major 2) that introduced the method: it is
    /// accepted only on a connection that negotiated at least this.
    pub since_minor: u64,
}

impl Method {
    /// Every method, in catalogue order. `tests/schema_agreement.rs`
    /// holds this list to the enum's own variants, so a variant missing
    /// here fails a test rather than escaping one.
    pub const ALL: [Self; 14] = [
        Self::ChannelJoin,
        Self::ChannelLeave,
        Self::BroadcastPublish,
        Self::DirectSend,
        Self::EndpointsQuery,
        Self::AdminStatus,
        Self::AdminEndpointsList,
        Self::AdminEndpointsRevoke,
        Self::AdminEndpointsSetEnabled,
        Self::AdminEndpointsSetDefault,
        Self::AdminShutdown,
        Self::AdminTrustList,
        Self::AdminTrustSet,
        Self::AdminPeersList,
    ];

    /// THE table: an exhaustive match, so a new method does not compile
    /// until it has a domain, a capability and a minor.
    #[must_use]
    pub const fn entry(self) -> MethodEntry {
        use AuthorityDomain::{Admin, Data};
        use RequestedCapability as C;
        let (domain, capability) = match self {
            Self::ChannelJoin | Self::ChannelLeave | Self::BroadcastPublish | Self::DirectSend => {
                (Data, C::Commands)
            }
            Self::EndpointsQuery => (Data, C::EndpointsQuery),
            Self::AdminStatus | Self::AdminPeersList => (Admin, C::AdminStatus),
            Self::AdminEndpointsList
            | Self::AdminEndpointsRevoke
            | Self::AdminEndpointsSetEnabled
            | Self::AdminEndpointsSetDefault => (Admin, C::AdminEndpoints),
            Self::AdminShutdown => (Admin, C::AdminShutdown),
            Self::AdminTrustList | Self::AdminTrustSet => (Admin, C::AdminTrust),
        };
        let since_minor = match self {
            Self::AdminTrustList | Self::AdminTrustSet => 1,
            Self::AdminPeersList => 2,
            _ => 0,
        };
        MethodEntry {
            domain,
            capability,
            since_minor,
        }
    }

    /// The method's wire name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ChannelJoin => "channel.join",
            Self::ChannelLeave => "channel.leave",
            Self::BroadcastPublish => "broadcast.publish",
            Self::DirectSend => "direct.send",
            Self::EndpointsQuery => "endpoints.query",
            Self::AdminStatus => "admin.status",
            Self::AdminEndpointsList => "admin.endpoints.list",
            Self::AdminEndpointsRevoke => "admin.endpoints.revoke",
            Self::AdminEndpointsSetEnabled => "admin.endpoints.set_enabled",
            Self::AdminEndpointsSetDefault => "admin.endpoints.set_default",
            Self::AdminShutdown => "admin.shutdown",
            Self::AdminTrustList => "admin.trust.list",
            Self::AdminTrustSet => "admin.trust.set",
            Self::AdminPeersList => "admin.peers.list",
        }
    }

    /// The method a wire name names, or `None` for a name outside the
    /// catalogue (answered `ProtocolUnsupported`).
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|m| m.as_str() == name)
    }

    /// Whether a connection that negotiated `version` may use the method:
    /// minors are additive, so a method is unknown below the minor that
    /// introduced it.
    #[must_use]
    pub const fn available_at(self, version: IpcVersion) -> bool {
        version.minor >= self.entry().since_minor
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_wire_name_is_the_serde_name_and_parses_back() {
        for method in Method::ALL {
            assert_eq!(
                serde_json::to_value(method).expect("ser"),
                serde_json::json!(method.as_str())
            );
            assert_eq!(Method::parse(method.as_str()), Some(method));
        }
        assert_eq!(Method::parse("admin.trust.add"), None);
        assert_eq!(Method::parse("Channel.Join"), None, "names are exact");
    }

    #[test]
    fn a_domain_and_its_capabilities_never_cross() {
        for method in Method::ALL {
            let entry = method.entry();
            let admin = entry.capability.as_admin().is_some();
            assert_eq!(
                admin,
                entry.domain == AuthorityDomain::Admin,
                "{}: an admin capability is served only on the admin socket",
                method.as_str()
            );
            assert_eq!(
                method.as_str().starts_with("admin."),
                admin,
                "{}",
                method.as_str()
            );
        }
    }

    #[test]
    fn a_method_is_unavailable_below_its_minor() {
        let at = |minor| IpcVersion { major: 2, minor };
        for method in Method::ALL {
            assert!(method.available_at(at(method.entry().since_minor)));
            if let Some(below) = method.entry().since_minor.checked_sub(1) {
                assert!(!method.available_at(at(below)));
            }
        }
    }

    #[test]
    fn the_trust_methods_are_admin_and_arrive_at_two_one() {
        for method in [Method::AdminTrustList, Method::AdminTrustSet] {
            let entry = method.entry();
            assert_eq!(entry.domain, AuthorityDomain::Admin);
            assert_eq!(entry.capability, RequestedCapability::AdminTrust);
            assert_eq!(entry.since_minor, 1);
        }
        assert_eq!(Method::AdminShutdown.entry().since_minor, 0, "the control");
    }

    /// A method never arrives before its capability: one stated at a
    /// lower minor than its capability could be asked on a connection
    /// that could not hold it. It may arrive AFTER it -- a later read
    /// under an existing capability, as `admin.peers.list` (2.2) is under
    /// `admin.status` (2.0) -- since a connection below the method's minor
    /// is answered as for an unknown method whatever it holds
    /// (`available_at`). Until 2.2 the two minors were equal for every
    /// method, which this test then pinned.
    #[test]
    fn no_method_arrives_before_its_capability() {
        for method in Method::ALL {
            let entry = method.entry();
            assert!(
                entry.since_minor >= entry.capability.since_minor(),
                "{}",
                method.as_str()
            );
        }
    }

    #[test]
    fn the_peers_read_is_admin_under_status_and_arrives_at_two_two() {
        let entry = Method::AdminPeersList.entry();
        assert_eq!(entry.domain, AuthorityDomain::Admin);
        assert_eq!(entry.capability, RequestedCapability::AdminStatus);
        assert_eq!(entry.since_minor, 2);
        let at = |minor| IpcVersion { major: 2, minor };
        assert!(
            !Method::AdminPeersList.available_at(at(1)),
            "unknown at 2.1"
        );
        assert!(Method::AdminPeersList.available_at(at(2)));
        assert!(Method::AdminStatus.available_at(at(0)), "the control");
    }
}
