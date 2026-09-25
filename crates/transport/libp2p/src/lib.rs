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
//! # The feature list withholds no behaviour any more
//!
//! This section used to name GossipSub, direct v2, Kademlia, AutoNAT,
//! Circuit Relay and DCUtR as absent from the `libp2p` feature list this
//! crate compiles against — not merely unused, so none could be switched
//! on by a `use` statement or a stray builder call, because the code was
//! not there. That was the cheapest way to keep CLAUDE.md §3's promise
//! that admission policy is never retrofitted, and each stage spent its
//! entry from that list exactly once.
//!
//! **Stage 11 spent the last three.** All six are compiled now, so the
//! promise is kept by the outbound gate, by the trust classification and
//! by their tests, and by nothing else. Two consequences a reader should
//! carry: a rule about an infrastructure-only peer is a rule about a
//! state this build can now reach rather than a latent one, and a new
//! behaviour added here is one nothing outside this crate prevents from
//! dialling.
//!
//! **`mdns` and `dns` joined them on 2026-09-20**, and neither is a
//! behaviour that dials on its own. Both had been held off the list
//! until then -- `mdns` first by RUSTSEC advisories inside the
//! `libp2p-mdns 0.48` line `libp2p 0.56` pinned, then, once PR #109's
//! bump cleared those, only by a stage decision nobody had taken.
//!
//! - `mdns`: the multicast mechanism is built --
//!   [`MdnsScope`](mdns_scope::MdnsScope) swallows the crate's address
//!   injections (ADR-0011: a discovery provider never writes the address
//!   book) and the driver applies ADR-0052's boundary where a candidate
//!   is learned. It is constructed only when `SubstrateConfig::mdns` is
//!   `Some`, which nothing does before
//!   Stage 12 composes providers. Stage 11's `mdns` deadline reads
//!   TAKEN-NOT-MET until SPIKE-010's multicast run: no packet has
//!   crossed a wire in this repository's tests.
//! - `dns`: the Swarm builder wraps the base transport in it, so a
//!   `/dns4` or `/dns6` name the OPERATOR configures resolves at dial,
//!   and `profile-config` accepts one. A name a PEER supplies does not:
//!   ADR-0052 (A 2026-09-20) refuses it where it would enter the address
//!   book or Kademlia's routing table, because to a peer the resolver is
//!   an oracle. Wrapping the whole transport also changed the error
//!   every dial reports, which is why an undialable address is now
//!   recognised by its own shape rather than by
//!   `TransportError::MultiaddrNotSupported`.
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
pub mod candidate_scope;
mod class_gate;
pub mod direct_codec;
pub mod endpoints_codec;
pub mod gated_swarm;
pub mod hole_punch;
pub mod mdns_scope;
pub mod operator_set;
pub mod outbound_gate;
pub mod preauth_gate;
pub mod probe_server;
pub mod refusals;
pub mod reservation_scope;
pub mod root_funnel;
pub mod runtime;
pub mod served_addresses;

pub use attribution::{Attributing, Classifier, DialAttribution, always};
pub use behaviour::{IDENTIFY_PROTOCOL_VERSION, SubstrateBehaviour};
pub use gated_swarm::{AdmittedDial, GatedSwarm};
pub use outbound_gate::{AdmittedDials, OutboundAdmission};
pub use preauth_gate::PreAuthAdmission;
pub use refusals::{DialRefusals, RECENT_CAPACITY, Refusal};
pub use runtime::{
    BroadcastChannels, DEFAULT_COMMAND_CAPACITY, DEFAULT_EVENT_CAPACITY, DialRefusal,
    HolePunchOutcome, MAX_CONFIGURED_CAPACITY, PathChange, PeerPath, RelayReservationOutcome,
    RelayServerOutcome, SubstrateConfig, SubstrateError, SwarmCommand, SwarmEvent, SwarmRuntime,
};
