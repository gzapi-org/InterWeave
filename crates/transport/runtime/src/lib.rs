// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! Backend-neutral transport runtime state machines.
//!
//! Stage 2 activates the PURE modules only: decisions and state that can
//! be tested by enumeration rather than orchestration. Nothing here opens
//! a socket, starts a Swarm, or reads a clock.

#![forbid(unsafe_code)]

pub mod connection_manager;
pub mod connection_policy;
pub mod dedup;
pub mod direct_inbound;
pub mod endpoint_queue;
pub mod endpoint_registry;
pub mod fingerprint;
pub mod ingress;
pub mod preauth;
pub mod reply_token;

pub use connection_manager::{
    ADMIT_RELOAD_ATTEMPTS, ConnectionManager, ConnectionSlot, DEFAULT_MAX_ADDRESSES_PER_PEER,
    DEFAULT_MAX_RETRY_ENTRIES, DialTicket, PolicySnapshot, Revoked, SnapshotHandle, TrustSources,
};
pub use connection_policy::{
    AddressState, ConnectionClass, ConnectionPolicy, DEFAULT_IDLE_TTL_MS,
    DEFAULT_MAX_ADDRESS_ENTRIES, DEFAULT_MAX_PEER_ENTRIES, DialDenial, DialOrigin, DialRequest,
    PeerBackoff,
};
pub use dedup::{
    Admission, DedupCache, DedupKey, DestinationSelector, Reservation, ReservationFailure,
    ReservationMap,
};
pub use direct_inbound::{AdmissionContext, Clocks, Outcome, Refusal, admit_inbound};
// RE-EXPORTED, so a backend composing admission does not have to name
// `trust-api` itself. `AdmissionContext` holds a `PeerTrustPolicy`, so
// the type is already in this crate's public API — a consumer that could
// not spell it could not build the context.
pub use endpoint_queue::{DirectEvent, EndpointQueues, QueueRefusal};
pub use endpoint_registry::{
    ActiveLease, ClaimFailure, EndpointRegistry, LocalSessionId, RegisteredEndpoint, ResolveFailure,
};
pub use fingerprint::{ContentFingerprint, FingerprintError, direct_content_fingerprint_v1};
pub use ingress::{
    IngressDenial, IngressLimiter, MAX_SESSIONS_PER_CHANNEL, MAX_SUBSCRIPTIONS, SubscriptionDenial,
    SubscriptionRegistry,
};
pub use interweave_trust_api::{EndpointTrustPolicy, PeerTrustPolicy};
// Same reason: `EndpointRegistry::claim` takes a `Generation`, so it is
// already in this crate's public API and a caller that could not spell
// it could not claim a lease.
pub use interweave_local_client_api::Generation;
pub use reply_token::{DuplicateToken, ReplyResolution, ReplyRoute, ReplyTokenTable};
