// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! What a default profile offers on the wire, as a third party sees it.
//!
//! This test exists because Stage 11 spent the last of the libp2p
//! feature list's protection. Until then, "relay is not compiled" was a
//! guarantee the compiler enforced, and a comment asserting it was
//! backed by `Cargo.toml`. `autonat`, `relay` and `dcutr` are compiled
//! now, so the only thing keeping their protocols off the wire is that
//! nothing constructs the behaviours — which is a property of the code,
//! and properties of the code need tests.
//!
//! The assertion is an EXACT SET rather than a series of absences.
//! Listing the three protocols this stage happens to be thinking about
//! would pass for any fourth behaviour someone constructs later, and the
//! failure this guards against is not specific to those three: it is a
//! behaviour reaching the Swarm without anyone deciding it should be
//! offered.
//!
//! # What this catches, measured rather than assumed
//!
//! Each row is a behaviour actually added to `SubstrateBehaviour` and
//! constructed, with the test then run. The first draft of this comment
//! claimed the exact set "fails for all of them"; the third row is why
//! that claim is gone.
//!
//! | constructed without configuration | caught? |
//! | --- | --- |
//! | `relay::Behaviour` (server) | YES — `/libp2p/circuit/relay/0.2.0/hop` appears |
//! | `autonat::v2::client::Behaviour` | YES — `/libp2p/autonat/2/dial-back` appears |
//! | `dcutr::Behaviour` | **NO — survives silently** |
//! | `relay::client::Behaviour` | yes, but by PANIC rather than by this assertion |
//!
//! **DCUtR is invisible here and that is structural, not a gap to
//! tighten.** `libp2p-dcutr 0.14.1` registers its relayed handler only
//! when `is_relayed(local_addr)` (`behaviour.rs:179`); on a direct
//! connection it installs a dummy handler and advertises nothing. So no
//! observer on a direct connection can see it, and this test cannot be
//! made to. Catching a constructed DCUtR needs an observer on a
//! `/p2p-circuit` connection, which needs a relay — Phase 4's work, and
//! recorded here so it is not mistaken for covered.
//!
//! The fourth row is a safeguard worth knowing: `relay::client::Behaviour`
//! cannot be half-installed. Constructed without its paired
//! `relay::client::Transport` on the Swarm, the first poll hits
//! `unreachable!("`relay::Behaviour` polled after channel from
//! `Transport` has been closed")`. A relay client therefore cannot be
//! added without also changing the builder chain.
//!
//! Read from a third-party Identify observer over loopback rather than
//! from this crate's own types, because what a peer is TOLD is the
//! question. A test that asked `SubstrateBehaviour` what it contains
//! would agree with any mistake made in constructing it.

#![allow(clippy::expect_used, clippy::panic)]

use std::collections::BTreeSet;
use std::time::Duration;

use interweave_profile_identity::ProfileIdentity;
use interweave_transport_api::TransportIdentity;
use interweave_transport_libp2p::{SubstrateConfig, SwarmRuntime};
use interweave_transport_runtime::TrustSources;
use interweave_trust_api::{InfrastructureSet, PeerTrustPolicy};
use libp2p::Multiaddr;

/// How long a loopback Identify exchange may take before the test fails.
const PATIENCE: Duration = Duration::from_secs(20);

/// Everything a default profile advertises, and nothing else.
///
/// **Every entry here is the BACKEND's, and that is the first thing this
/// test taught.** It was written asserting `/interweave/id/1.0.0` among
/// them, on the strength of a constant then called `IDENTIFY_PROTOCOL`
/// and documented as "the Identify protocol name this profile
/// advertises". It is not — it is now `IDENTIFY_PROTOCOL_VERSION`,
/// renamed because of this test: `identify::Config::new` takes a
/// `protocol_version` string, which travels inside the Identify payload
/// as metadata, while the negotiated names are libp2p's own hardcoded
/// `/ipfs/id/1.0.0` and `/ipfs/id/push/1.0.0`
/// (`libp2p-identify-0.47.0` `protocol.rs:35,37`). The three
/// `/meshsub/` entries are GossipSub's, likewise — and there are three,
/// not the two a reader of `PUBSUB.md` would predict.
///
/// So the two InterWeave protocols below are the only ones this project
/// names. A test that had asserted the set it EXPECTED would have been
/// edited until it passed; this one was written first and corrected by
/// what came back.
///
/// **Adding a line here is a decision, not a fix.** Every entry is a
/// protocol this node offers to any peer that connects, before any
/// authority check runs — the exposure `BOTTOM-UP-IMPLEMENTATION-PLAN.md`
/// §14 requires be restricted at the connection for an
/// infrastructure-only peer. A new one arriving without that
/// restriction is the gap that invariant names.
const ADVERTISED: &[&str] = &[
    "/interweave/direct/2.0.0",
    "/interweave/endpoints/1.0.0",
    "/ipfs/id/1.0.0",
    "/ipfs/id/push/1.0.0",
    "/meshsub/1.0.0",
    "/meshsub/1.1.0",
    "/meshsub/1.2.0",
];

/// A profile that trusts exactly `peers` and treats nobody as
/// infrastructure.
fn trusting(peers: &[&TransportIdentity]) -> TrustSources {
    TrustSources::new(
        PeerTrustPolicy::new(peers.iter().map(|p| (*p).clone())).expect("a small allowlist"),
        InfrastructureSet::default(),
    )
}

/// Start a runtime and wait for the address it actually bound.
async fn listening(runtime: &mut SwarmRuntime) -> Multiaddr {
    runtime
        .listen(
            "/ip4/127.0.0.1/tcp/0"
                .parse()
                .expect("a valid listen address"),
        )
        .await
        .expect("the listener binds")
}

/// Connect a bare Identify swarm to `target` and report what it is told.
///
/// Deliberately NOT a `SwarmRuntime`: this observer must speak nothing
/// but Identify, so the protocol list it receives is the subject's
/// advertisement rather than an intersection with the observer's own.
async fn advertised_protocols(
    keys: libp2p::identity::Keypair,
    target: &TransportIdentity,
    address: Multiaddr,
) -> BTreeSet<String> {
    use futures::StreamExt as _;

    let mut observer = libp2p::SwarmBuilder::with_existing_identity(keys)
        .with_tokio()
        .with_tcp(
            libp2p::tcp::Config::default(),
            libp2p::noise::Config::new,
            libp2p::yamux::Config::default,
        )
        .expect("the same transport stack the subject uses")
        .with_behaviour(|k| {
            libp2p::identify::Behaviour::new(libp2p::identify::Config::new(
                "/interweave-protocol-set-observer/1".to_owned(),
                k.public(),
            ))
        })
        .expect("behaviour")
        .build();

    let peer: libp2p::PeerId = target.as_str().parse().expect("a libp2p identity");
    observer
        .dial(
            libp2p::swarm::dial_opts::DialOpts::peer_id(peer)
                .addresses(vec![address])
                .build(),
        )
        .expect("dial accepted");

    let deadline = tokio::time::Instant::now() + PATIENCE;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(!remaining.is_zero(), "no Identify arrived from the subject");
        match tokio::time::timeout(remaining, observer.select_next_some()).await {
            Ok(libp2p::swarm::SwarmEvent::Behaviour(libp2p::identify::Event::Received {
                info,
                ..
            })) => {
                return info.protocols.iter().map(ToString::to_string).collect();
            }
            Ok(_) => {}
            Err(_) => panic!("no Identify arrived from the subject"),
        }
    }
}

#[tokio::test]
async fn a_default_profile_advertises_exactly_these_protocols_and_no_others() {
    // The observer's identity has to exist before the subject does: the
    // subject must TRUST it, or the connection closes before Identify
    // completes and an empty protocol list would mean "no conversation"
    // rather than "no protocols".
    let observer_keys = libp2p::identity::Keypair::generate_ed25519();
    let observer_peer = TransportIdentity::parse(observer_keys.public().to_peer_id().to_base58())
        .expect("a canonical identity");

    let subject_id = ProfileIdentity::generate();
    let subject_peer = subject_id.transport_identity().expect("peer id");

    // A DEFAULT configuration: `SubstrateConfig::default()` leaves
    // `kademlia: None`, which is the profile a node with no opinion
    // gets. If a connectivity behaviour is ever constructed without a
    // configuration asking for it, this is the profile that shows it.
    let mut subject = SwarmRuntime::start(
        &subject_id,
        SubstrateConfig::default(),
        trusting(&[&observer_peer]),
    )
    .expect("the runtime starts");
    let address = listening(&mut subject).await;

    let advertised = advertised_protocols(observer_keys, &subject_peer, address).await;
    let expected: BTreeSet<String> = ADVERTISED.iter().map(|s| (*s).to_owned()).collect();

    assert_eq!(
        advertised, expected,
        "a default profile must advertise exactly the protocols it was built with. \
         Extra entries mean a behaviour was constructed that nobody configured — \
         since Stage 11 compiled `autonat`, `relay` and `dcutr`, that is now \
         possible without a manifest change. Missing entries mean a protocol \
         stopped being offered."
    );

    subject.shutdown().await.expect("stops");
}
