// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! Backend-neutral transport runtime state machines.
//!
//! Stage 2 activates the PURE modules only: decisions and state that can
//! be tested by enumeration rather than orchestration. Nothing here opens
//! a socket, starts a Swarm, or reads a clock.

#![forbid(unsafe_code)]

pub mod connection_policy;
pub mod dedup;
pub mod endpoint_registry;
pub mod fingerprint;
pub mod ingress;
pub mod reply_token;

pub use connection_policy::{
    AddressState, ConnectionClass, ConnectionPolicy, DialDenial, DialOrigin, DialRequest,
    PeerBackoff,
};
pub use dedup::{
    Admission, DedupCache, DedupKey, DestinationSelector, Reservation, ReservationFailure,
    ReservationMap,
};
pub use endpoint_registry::{
    ActiveLease, ClaimFailure, EndpointRegistry, LocalSessionId, RegisteredEndpoint, ResolveFailure,
};
pub use fingerprint::{ContentFingerprint, FingerprintError, direct_content_fingerprint_v1};
pub use ingress::{IngressDenial, IngressLimiter, SubscriptionRegistry};
pub use reply_token::{ReplyResolution, ReplyRoute, ReplyTokenTable};
