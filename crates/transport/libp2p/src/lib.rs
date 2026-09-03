// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The authenticated transport substrate.
//!
//! Stage 4 of the canonical plan built the substrate: TCP, Noise,
//! Yamux, Identify, and nothing else. Two peers can listen, dial,
//! authenticate each other's PeerId, exchange Identify, and shut down.
//! No application protocol runs over it yet.
//!
//! Stage 5 added the funnel around it. Outbound, [`GatedSwarm::dial`]
//! takes an [`AdmittedDial`], which is derived from a ticket only the
//! root `ConnectionManager` issues. Inbound,
//! [`preauth_gate::PreAuthAdmission`] answers before the Noise upgrade
//! begins, so the work an unauthenticated party can make this process
//! do is bounded where libp2p can still say no. Once a peer HAS
//! authenticated it is classified from the profile's trust sources,
//! which is why [`SwarmRuntime::start`] takes them: a runtime that
//! could be started without them would have a window in which it
//! trusted everybody.
//!
//! [`outbound_gate::OutboundAdmission`] closes the third door: libp2p
//! routes a behaviour's own dials through
//! `handle_pending_outbound_connection` rather than through
//! [`GatedSwarm`]. A dial carrying a ticket passes on it; one without —
//! behaviour-originated, by definition — is admitted through the SAME
//! root policy under `DialOrigin::KademliaQuery`, its ticket deposited
//! for the ordinary settlement path, and its address judged at the
//! established hook where one first exists. The gate existed and
//! refused everything BEFORE any behaviour that dials, which is the
//! order CLAUDE.md §3 requires; Stage 10 taught it to answer with
//! policy rather than with a flat no.
//!
//! # What is absent, and why it is absent rather than merely unused
//!
//! GossipSub, direct v2, Kademlia, AutoNAT, Circuit Relay and DCUtR are
//! not in the `libp2p` feature list this crate compiles against. They
//! cannot be switched on by a `use` statement or a stray builder call,
//! because the code is not there. CLAUDE.md §3 forbids enabling
//! autonomous libp2p behaviour and retrofitting admission policy
//! afterwards, and the cheapest way to keep that promise is to not
//! compile the behaviour.
//!
//! # Nothing above this crate sees a libp2p type
//!
//! The boundary speaks [`interweave_transport_api::TransportIdentity`]
//! and this crate's own [`SwarmCommand`] and [`SwarmEvent`]. That keeps
//! the backend replaceable and keeps `crates/api/*` free of libp2p
//! (CLAUDE.md §4) — the translation happens once, here.
//!
//! # Dials are admitted, from the first line of substrate code
//!
//! [`SwarmRuntime`] runs every dial through
//! [`interweave_transport_runtime::ConnectionPolicy`], which Stage 2
//! implemented and tested, and Stage 5 made that gate *root*: the raw
//! `Swarm` is private to [`GatedSwarm`], so a call site that forgets to
//! ask does not misbehave at runtime — it does not compile.
//!
//! The behaviour path is gated the same way: every dial a behaviour
//! originates is decided by the root policy inside
//! `NetworkBehaviour::handle_pending_outbound_connection`, before a
//! socket is opened — which is what CLAUDE.md §3 required to be green
//! before Kademlia could be activated, and it is green first: the
//! `kad` feature arrives only in the commit after this gate's tests.

#![forbid(unsafe_code)]

pub mod attribution;
pub mod behaviour;
pub mod direct_codec;
pub mod endpoints_codec;
pub mod gated_swarm;
pub mod outbound_gate;
pub mod preauth_gate;
pub mod refusals;
pub mod runtime;

pub use attribution::{Attributing, Classifier, DialAttribution, always};
pub use behaviour::{IDENTIFY_PROTOCOL, SubstrateBehaviour};
pub use gated_swarm::{AdmittedDial, GatedSwarm};
pub use outbound_gate::{AdmittedDials, OutboundAdmission};
pub use preauth_gate::PreAuthAdmission;
pub use refusals::{DialRefusals, RECENT_CAPACITY, Refusal};
pub use runtime::{
    BroadcastChannels, DEFAULT_COMMAND_CAPACITY, DEFAULT_EVENT_CAPACITY, DialRefusal,
    MAX_CONFIGURED_CAPACITY, SubstrateConfig, SubstrateError, SwarmCommand, SwarmEvent,
    SwarmRuntime,
};
