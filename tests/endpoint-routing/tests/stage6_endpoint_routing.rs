// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! Stage 6 exit gate, the routing half: same-PeerId multi-endpoint
//! delivery (ADR-0030 Model B).
//!
//! The network scenarios land with the backend. What is here at the
//! opening commit is the one claim already true: the registry this
//! stage routes through resolves an explicit destination and a default
//! deterministically, and collapses every failure to the same coarse
//! wire code. That is the invariant the network tests will later assert
//! from the other side of a socket; asserting it here first means a
//! regression in the registry is caught as a registry defect, not
//! discovered as a mysterious `no_route`.

use interweave_transport_api::{DirectRejectReason, EndpointId};
use interweave_transport_runtime::endpoint_registry::ResolveFailure;

/// Every local resolve failure is the SAME wire code. A `const fn`
/// returning one value cannot grow a distinct code by having a variant
/// added, which is the property the exit gate's scenario 6 depends on.
#[test]
fn every_resolve_failure_is_no_route_on_the_wire() {
    for failure in [
        ResolveFailure::EndpointUnknown,
        ResolveFailure::EndpointDisabled,
        ResolveFailure::EndpointOffline,
        ResolveFailure::NoDefaultConfigured,
        ResolveFailure::EndpointPolicyDenied,
    ] {
        assert_eq!(
            failure.to_wire(),
            DirectRejectReason::NoRoute,
            "{failure:?} must not be distinguishable remotely"
        );
    }
}

/// The endpoint grammar the frame carries is the one the registry keys
/// on: the 64-byte ceiling and the leading-lowercase rule are shared,
/// so a name the codec accepts is a name the registry can hold.
#[test]
fn endpoint_ids_share_one_grammar_between_wire_and_registry() {
    assert_eq!(EndpointId::MAX_BYTES, 64);
    assert!(EndpointId::parse("a".repeat(64)).is_ok());
    assert!(EndpointId::parse("a".repeat(65)).is_err());
    assert!(EndpointId::parse("Human").is_err(), "must begin a-z");
    assert!(EndpointId::parse("human").is_ok());
}
