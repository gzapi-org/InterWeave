// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! InterWeave's transport runtime, composed from a validated profile
//! (plan §15, Stage 12).
//!
//! [`ComposedRuntime::start`] is the production caller of
//! `SwarmRuntime::start`: the profile is validated and translated
//! (`translate`), each behaviour its blocks enable is switched on, the
//! configured discovery providers are composed into one
//! `DiscoveryManager` whose output reaches the address book through the
//! peer's door (`discovery`), and the result is driven by one task behind
//! the neutral [`interweave_transport_api::TransportRuntime`] surface
//! (`runtime`). No libp2p type crosses that trait.

pub mod discovery;
pub mod gate;
mod notices;
pub mod runtime;
pub mod session;
pub mod translate;

pub use discovery::{DiscoveryDiagnostics, ProviderDiagnostics};
pub use gate::{LastOutcome, PeerGateRow};
/// The grace a stop gives exchanges already in flight when it names none
/// (`ComposedRuntime::stop`): the substrate's, re-exported so a
/// composition root names it without depending on the backend.
pub use interweave_transport_libp2p::runtime::SHUTDOWN_GRACE;
pub use notices::{MAX_PEER_NOTICES, MAX_ROUTED_PEERS, PeerNoticeDiagnostics};
pub use runtime::{
    AUDIT_TARGET, ComposedRuntime, CompositionOptions, Diagnostics, ShutdownRequest,
};
pub use session::{InProcessAdmin, InProcessBinding, InProcessSession};
pub use translate::{Composition, CompositionError, DiscoveryPlan, translate};
