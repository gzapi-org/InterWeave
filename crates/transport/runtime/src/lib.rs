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
pub mod preauth;
pub mod reply_token;

pub use connection_policy::{
    AddressState, ConnectionClass, ConnectionPolicy, DEFAULT_IDLE_TTL_MS,
    DEFAULT_MAX_ADDRESS_ENTRIES, DEFAULT_MAX_PEER_ENTRIES, DialDenial, DialOrigin, DialRequest,
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
pub use ingress::{
    IngressDenial, IngressLimiter, MAX_SESSIONS_PER_CHANNEL, MAX_SUBSCRIPTIONS, SubscriptionDenial,
    SubscriptionRegistry,
};
pub use reply_token::{DuplicateToken, ReplyResolution, ReplyRoute, ReplyTokenTable};
