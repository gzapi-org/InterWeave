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
pub mod runtime;
pub mod session;
pub mod translate;

pub use discovery::{DiscoveryDiagnostics, ProviderDiagnostics};
pub use runtime::{ComposedRuntime, CompositionOptions, Diagnostics};
pub use session::{InProcessAdmin, InProcessBinding, InProcessSession};
pub use translate::{Composition, CompositionError, DiscoveryPlan, translate};
