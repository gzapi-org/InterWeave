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
#![allow(clippy::expect_used, clippy::panic)]

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

/// ENDPOINT NAMES ARE AN OPEN SET, and this stage must not quietly close
/// it. `human` and `claude` are configured labels, not variants: a
/// profile that adds `gpt-5`, `gemini`, or `llama-4` is editing config,
/// not this crate, and no wire byte changes because the frame carries a
/// length-prefixed label rather than a code point.
///
/// The risk is not today's code — it is a future `match name { "human"
/// => .., "claude" => .. }` somewhere in routing, which would compile,
/// pass every existing test, and silently make unknown endpoints
/// unroutable. This asserts the property directly so that change fails
/// here.
#[test]
fn an_endpoint_name_no_one_has_heard_of_is_as_valid_as_the_familiar_ones() {
    for name in [
        "human",
        "claude",
        // Models that do not exist yet, and one that never will.
        "gpt-5",
        "gemini",
        "llama-4",
        "some.future.llm",
        "a",
    ] {
        assert!(
            EndpointId::parse(name).is_ok(),
            "`{name}` must be a legal endpoint label"
        );
    }

    // The grammar is what constrains a label — never a known-names list.
    assert!(EndpointId::parse("gpt 5").is_err(), "spaces are not legal");
    assert!(EndpointId::parse("GPT-5").is_err(), "must begin a-z");
}
