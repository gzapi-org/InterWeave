// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The Stage 2 exit-gate claims that span more than one crate.
//!
//! Each module tests its own rules. These are the statements no single
//! crate can make alone — a reader would otherwise have to hold two
//! crates in mind and trust that the seam between them holds.

#![allow(clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;

use interweave_ipc_protocol::{
    AuthorityDomain, ClientInfo, Hello, HelloTag, IpcVersion, RequestedCapability,
};
use interweave_local_client_api::{DataCapability, EndpointLease, Generation, LocalDataSession};
use interweave_transport_api::{DirectRejectReason, EndpointId, TransportIdentity};
use interweave_transport_runtime::{
    ConnectionClass, ConnectionPolicy, DialOrigin, DialRequest, EndpointRegistry,
    RegisteredEndpoint, ResolveFailure,
};
use interweave_trust_api::{EndpointTrustPolicy, PeerTrustPolicy};

const P1: &str = "12D3KooWDpJ7As7BWAwRMfu1VU2WCqNjvq387JEYKDBj4kx6nXTN";
const P2: &str = "12D3KooWK99VoVxNE7XzyBwXEzW7xhK7Gpv85r9F3V3fyKSUKPH5";

fn peer(s: &str) -> TransportIdentity {
    TransportIdentity::parse(s).expect("valid identity")
}
fn ep(n: &str) -> EndpointId {
    EndpointId::parse(n).expect("valid endpoint")
}
fn generation(seed: &str) -> Generation {
    Generation::parse(format!("{seed:_<16}")).expect("valid generation")
}

fn hello(kind: &str, caps: &[RequestedCapability]) -> Hello {
    Hello {
        frame_type: HelloTag::Hello,
        ipc_version: IpcVersion { major: 2, minor: 0 },
        client: ClientInfo {
            kind: kind.to_owned(),
            version: None,
        },
        endpoint: None,
        requested_capabilities: caps.iter().copied().collect(),
        features: Default::default(),
    }
}

#[test]
fn client_kind_cannot_widen_the_admin_data_intersection() {
    // The exit-gate bullet, end to end across ipc-protocol and
    // local-client-api. Three claims, each of which would be enough on
    // its own to break the split if it failed.

    // 1. The handshake refuses admin.* on the data socket however the
    //    client labels itself. The label is not an input to the decision.
    for kind in [
        "admin",
        "administrator",
        "settings",
        "root",
        "claude-channel",
    ] {
        let h = hello(kind, &[RequestedCapability::AdminShutdown]);
        assert!(
            h.evaluate(AuthorityDomain::Data, false).is_err(),
            "client_kind {kind:?} obtained admin authority on the data socket"
        );
    }

    // 2. The same frame on the admin socket succeeds — proving the refusal
    //    above came from the SOCKET and not from anything about the label.
    let h = hello("admin", &[RequestedCapability::AdminShutdown]);
    let granted = h
        .evaluate(AuthorityDomain::Admin, false)
        .expect("the admin socket grants it");
    assert!(granted.granted_data.is_empty());
    assert!(!granted.granted_admin.is_empty());

    // 3. And a data session cannot represent the authority even if some
    //    future caller tried to construct one holding it: the granted set
    //    is DataCapability, which has no admin variant.
    let session = LocalDataSession::new(
        generation("sess"),
        "admin",
        Some(EndpointLease {
            endpoint: ep("human"),
            epoch: generation("epoch"),
        }),
        [DataCapability::Events, DataCapability::Commands],
        256,
    )
    .expect("valid session");
    assert_eq!(session.client_kind(), "admin");
    for c in session.capabilities() {
        let name = serde_json::to_value(c).expect("ser");
        let name = name.as_str().expect("string");
        assert!(!name.starts_with("admin."), "a data session holds {name}");
    }
}

#[test]
fn a_remote_source_endpoint_claims_no_identity_or_authority() {
    // Spans trust-api and the runtime registry: a peer-asserted endpoint
    // name changes neither trust nor routing authority.
    let profile = PeerTrustPolicy::new([peer(P1)]).expect("within ceiling");

    let mut endpoints = BTreeMap::new();
    endpoints.insert(ep("human"), RegisteredEndpoint::default());
    let registry = EndpointRegistry::new(endpoints, Some(ep("human")));

    // An untrusted peer stays untrusted whatever endpoint it claims to be
    // sending from — the source endpoint is not an input to the decision.
    assert!(
        !registry
            .authorize_outbound(&ep("human"), &peer(P2), &profile)
            .is_allowed()
    );

    // And a narrowing endpoint policy cannot be widened by the remote
    // naming itself something else: the profile answer comes first.
    let mut narrowed = BTreeMap::new();
    narrowed.insert(
        ep("human"),
        RegisteredEndpoint {
            outbound: EndpointTrustPolicy::StaticSubset {
                allowed_peers: [peer(P2)].into_iter().collect(),
            },
            ..RegisteredEndpoint::default()
        },
    );
    let narrowed = EndpointRegistry::new(narrowed, None);
    assert!(
        !narrowed
            .authorize_outbound(&ep("human"), &peer(P2), &profile)
            .is_allowed(),
        "an endpoint subset must not authorize a peer the profile refused"
    );
}

#[test]
fn every_local_route_failure_is_one_coarse_answer_on_the_wire() {
    // Spans the runtime registry and transport-api's wire vocabulary.
    // Locally an operator sees five distinct reasons; a remote peer sees
    // one, which is what stops the protocol being an endpoint oracle.
    let locals = [
        ResolveFailure::EndpointUnknown,
        ResolveFailure::EndpointDisabled,
        ResolveFailure::EndpointOffline,
        ResolveFailure::NoDefaultConfigured,
        ResolveFailure::EndpointPolicyDenied,
    ];
    let wire: Vec<DirectRejectReason> = locals.iter().map(|f| f.to_wire()).collect();
    assert!(
        wire.iter().all(|w| *w == DirectRejectReason::NoRoute),
        "a local reason leaked a distinct wire code"
    );
    // Five distinct local reasons, one wire answer.
    let mut distinct = locals.to_vec();
    distinct.dedup();
    assert_eq!(distinct.len(), 5);
}

#[test]
fn an_infrastructure_peer_cannot_reach_the_data_plane_by_any_origin() {
    // Spans trust-api's InfrastructureSet concept and the runtime gate:
    // reachability authorization is not a weaker data-plane trust.
    let policy = ConnectionPolicy::new(16, 64);
    let address = "/ip4/192.0.2.1/tcp/4001".to_owned();

    for origin in [DialOrigin::ConnectionManager, DialOrigin::KademliaQuery] {
        let request = DialRequest {
            peer: Some(peer(P2)),
            address: address.clone(),
            origin,
        };
        assert!(
            policy
                .admit(&request, ConnectionClass::ConnectivityInfrastructureOnly, 0)
                .is_err(),
            "{origin:?} reached the data plane on an infrastructure-only class"
        );
    }

    // The reachability origins do work, so the refusals above are about
    // the data plane and not about the peer being unreachable.
    for origin in [
        DialOrigin::RelayReservation,
        DialOrigin::RelayCircuit,
        DialOrigin::AutonatProbe,
        DialOrigin::DcutrHolePunch,
    ] {
        let request = DialRequest {
            peer: Some(peer(P2)),
            address: address.clone(),
            origin,
        };
        assert!(
            policy
                .admit(&request, ConnectionClass::ConnectivityInfrastructureOnly, 0)
                .is_ok(),
            "{origin:?} should be permitted for an infrastructure peer"
        );
    }
}
