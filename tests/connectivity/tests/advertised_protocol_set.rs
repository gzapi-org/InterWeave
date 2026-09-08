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
//! **A second blind spot, and it is about DIRECTION rather than about
//! any one behaviour.** The observer dials the subject, so what is read
//! is the subject's INBOUND handler set. A behaviour that installs a
//! real handler only in `handle_established_outbound_connection` would
//! be invisible here too. All five candidates checked for the table
//! register their protocol-bearing handlers on the inbound side, so no
//! row changes — but a future one need not. (Five, not four: the table
//! has a row per outcome, and `relay::Behaviour` and
//! `relay::client::Behaviour` were checked separately from the two
//! autonat halves.)
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
//! The fourth row needs its condition stated. The panic fires when the
//! paired `relay::client::Transport` has been DROPPED — which is what
//! the natural mistake, `relay::client::new(pid).1`, does — and
//! `priv_client.rs:377` says so in its own message: "never polled after
//! `client::Transport` is dropped". A Transport constructed and kept
//! alive but simply not installed on the Swarm does not close the
//! channel, and then the behaviour advertises
//! `/libp2p/circuit/relay/0.2.0/stop` on a direct inbound and the
//! exact-set assertion catches it instead. Either way it is caught; the
//! row records which mechanism does the catching.
//!
//! One caveat on how the panic PRESENTS. It fires inside the subject's
//! spawned Swarm task, and a panic there does not propagate to the test
//! thread — so what a reader actually sees is this file's `PATIENCE`
//! timeout, with the panic in the captured output. Caught, but not
//! self-explaining.
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

/// The observer's own idle-connection timeout.
///
/// Deliberately longer than [`PATIENCE`], so that a `ConnectionClosed`
/// seen inside the test window is necessarily the SUBJECT's decision.
/// libp2p's default is 10s, which is shorter than `PATIENCE` — and that
/// gap made the refusal assertion below unfalsifiable until it was
/// mutated.
///
/// **This covers only the OBSERVER's half of that confound.** The
/// subject's own idle timeout is `SubstrateConfig::default().idle_timeout`,
/// 60s against a 20s `PATIENCE`, so it cannot fire inside the window
/// either — but that is a default this test does not set and does not
/// assert. Lower it below `PATIENCE` and the refusal assertion goes
/// unfalsifiable again, for exactly the reason the first draft did.
///
/// The test also does not check WHY the subject closed. A refusal on
/// `max_connections` would satisfy it just as a trust refusal does; that
/// ceiling is 256 by default, so it cannot be what fires here, but the
/// assertion is about the closing rather than its reason.
const IDLE_FAR_BEYOND_PATIENCE: Duration = Duration::from_secs(600);

/// Everything a default profile advertises, and nothing else.
///
/// **Five of the seven entries are the BACKEND's, and which two are not
/// is the first thing this test taught.** It was written asserting `/interweave/id/1.0.0` among
/// them, on the strength of a constant then called `IDENTIFY_PROTOCOL`
/// and documented as "the Identify protocol name this profile
/// advertises". It is not — it is now `IDENTIFY_PROTOCOL_VERSION`,
/// renamed because of this test: `identify::Config::new` takes a
/// `protocol_version` string, which travels inside the Identify payload
/// as metadata, while the negotiated names are libp2p's own hardcoded
/// `/ipfs/id/1.0.0` and `/ipfs/id/push/1.0.0`
/// (`libp2p-identify-0.47.0` `protocol.rs:35,37`). The three
/// `/meshsub/` entries are GossipSub's, likewise — and there are three,
/// not the two `libp2p-gossipsub-0.49.5` `config.rs:572` names as the
/// default; `protocol.rs:47,52,56` registers all three.
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

/// A profile that treats `infra` as reachability infrastructure only and
/// trusts nobody for the data plane.
fn infrastructure_only(infra: &TransportIdentity) -> TrustSources {
    TrustSources::new(
        PeerTrustPolicy::new(std::iter::empty()).expect("an empty allowlist"),
        InfrastructureSet::new([infra.clone()]).expect("a one-peer set"),
    )
}

#[tokio::test]
async fn an_infrastructure_only_peer_gets_a_connection_established_before_it_is_refused() {
    // WHAT THIS CORRECTS. Several places in this repository said, in
    // effect, that while relay, AutoNAT and DCUtR were uncompiled no
    // `ConnectivityInfrastructureOnly` connection could be established
    // at all. (Not a count and not a quotation: an earlier version gave
    // "three places" and a verbatim phrase, and both were wrong --
    // `gated_swarm.rs` warns against restating a hit count for exactly
    // this reason, in this same crate.) That is not what the code does, and the difference
    // is the whole of `BOTTOM-UP-IMPLEMENTATION-PLAN.md` §14.
    //
    // Neither gate denies at the established INBOUND hook:
    // `PreAuthAdmission` and `OutboundAdmission` both return
    // `Ok(dummy::ConnectionHandler)` unconditionally there
    // (`preauth_gate.rs`, `outbound_gate.rs`), and pre-Noise admission
    // cannot know a PeerId in any case. **That is background, and this
    // test does not enforce it**: if a gate began denying there, the
    // observer would still see an establish followed by a close and this
    // test would pass unchanged. What it does catch is refusal BEFORE
    // establishment, and retention. The OUTBOUND hook is not like
    // this — `OutboundAdmission::handle_established_outbound_connection`
    // rebinds the address and can refuse — which is why this test is
    // written from the inbound side. So the
    // connection completes, every data-plane handler is installed, and
    // only afterwards does the runtime classify the peer and close it
    // (`runtime/dialing.rs`).
    //
    // The accurate word is RETAINED, not established. This test is what
    // makes that word load-bearing rather than a preference: it fails if
    // an infrastructure-only peer is ever refused before establishment,
    // which is what the old sentence claimed already happened, and it
    // fails if such a peer is kept.
    //
    // # What this window is NOT
    //
    // It is tempting to call this "the exposure `ClassGated<B>` exists to
    // close", and an earlier version of the surrounding prose did.
    // MEASURED, IT IS NOT: instrumenting this test to record any
    // `identify::Event::Received` before the close returned `None` on
    // five runs out of five. The runtime pushes the refusal on the same
    // `ConnectionEstablished` the Swarm emitted and closes in the same
    // loop iteration, so no substream is negotiated and nothing is
    // advertised in practice. The handlers are installed — that follows
    // from the established hook returning `Ok` — but installed is not
    // spoken.
    //
    // So what this pins is narrower and worth being exact about: an
    // infrastructure-only peer's connection **completes** rather than
    // being refused at or before the handshake. The §14 exposure proper
    // is about a connection that is KEPT, and there are THREE routes to
    // one -- CLAUDE.md §1 enumerates them and is the place to read them,
    // because paraphrasing that list is what went wrong in four
    // successive review rounds. This paragraph carried one of those
    // wrong paraphrases: it said a call site was needed and explicitly
    // denied the behaviour route, which is the intended route for both
    // reachability origins. This test is the control that
    // will make the RETAINED case meaningful when it becomes
    // reachable, and it is the negative case `ClassGated<B>` must flip.
    //
    // # This asserts today's behaviour, not a rule from ADR-0036
    //
    // Closing an infrastructure-only inbound is not something any
    // accepted document requires. ADR-0036 authorizes such a peer for
    // reachability control, and §14 requires that its offered protocol
    // set be RESTRICTED at the connection — which presupposes a
    // connection that exists and is kept. It is closed today only
    // because inbound asks the origin-less `ConnectionManager::authorizes`
    // (`dialing.rs`), which asks under `DialOrigin::Manual`.
    //
    // **There is an unresolved tension here and it is named rather than
    // decided** (CLAUDE.md §2). `transport/libp2p/CONNECTIVITY.md`'s
    // protocol matrix gives Identify and bounded ping a `yes` in the
    // infrastructure-only column, and ADR-0036 opens a clause with "on
    // an established infrastructure-only connection:". Both describe a
    // state this build cannot hold, since the connection is closed in
    // the same loop iteration as `ConnectionEstablished`. Whether the
    // documents or the code move is step 3's decision, not this test's.
    //
    // **Step 3 will have to change this**, and this test with it: an
    // AutoNAT v2 dial-back arrives as an inbound connection FROM the
    // infrastructure-only server, and the node that must serve
    // `/libp2p/autonat/2/dial-back` on it is the CLIENT --
    // `libp2p-autonat 0.15.0` installs `dial_back::Handler` on every
    // established inbound. Step 3 is the AutoNAT client; step 4 is the
    // server role. Under today's rule the client closes the dial-back
    // before it can answer. The same asymmetry
    // `settle_established_outbound` already documents on the outbound
    // side.
    use futures::StreamExt as _;

    let observer_keys = libp2p::identity::Keypair::generate_ed25519();
    let observer_peer = TransportIdentity::parse(observer_keys.public().to_peer_id().to_base58())
        .expect("a canonical identity");

    let subject_id = ProfileIdentity::generate();
    let subject_peer = subject_id.transport_identity().expect("peer id");

    let mut subject = SwarmRuntime::start(
        &subject_id,
        SubstrateConfig::default(),
        // The observer is infrastructure, and nothing else. Under
        // ADR-0036 that authorizes reachability control and no data
        // plane at all.
        infrastructure_only(&observer_peer),
    )
    .expect("the runtime starts");
    let address = listening(&mut subject).await;

    let mut observer = libp2p::SwarmBuilder::with_existing_identity(observer_keys)
        .with_tokio()
        .with_tcp(
            libp2p::tcp::Config::default(),
            libp2p::noise::Config::new,
            libp2p::yamux::Config::default,
        )
        .expect("the same transport stack the subject uses")
        .with_behaviour(|k| {
            libp2p::identify::Behaviour::new(libp2p::identify::Config::new(
                "/interweave-infra-observer/1".to_owned(),
                k.public(),
            ))
        })
        .expect("behaviour")
        // THE OBSERVER MUST NOT CLOSE THE CONNECTION ITSELF, or this
        // test cannot tell a refusal from a timeout. libp2p's default
        // idle timeout is 10s and `PATIENCE` is 20s, so the first draft
        // of this test passed for a DATA-PLANE-TRUSTED observer too --
        // the close it observed was its own. Caught by mutating the
        // trust source and watching the run go from 0.07s to 10.06s
        // while still reporting success.
        .with_swarm_config(|c| c.with_idle_connection_timeout(IDLE_FAR_BEYOND_PATIENCE))
        .build();

    let peer: libp2p::PeerId = subject_peer.as_str().parse().expect("a libp2p identity");
    observer
        .dial(
            libp2p::swarm::dial_opts::DialOpts::peer_id(peer)
                .addresses(vec![address])
                .build(),
        )
        .expect("dial accepted");

    // Both events are certain and ordered, so this is not a race: the
    // transport completes the handshake before the runtime's event loop
    // ever sees `ConnectionEstablished` to classify.
    let mut established = false;
    // Kept for the TIMEOUT DIAGNOSTIC, not for an assertion. Retention
    // is what lands in that arm, and what a retained peer was told is
    // the single most useful thing to print there.
    let mut advertised: Option<Vec<String>> = None;
    let deadline = tokio::time::Instant::now() + PATIENCE;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        // The same diagnostic as the timeout arm below, because this
        // fires on the knife-edge path where an event is delivered in
        // the instant the deadline expires: `Ok(_)` repeats the loop and
        // `saturating_duration_since` is then zero. An earlier version
        // printed only `established` here, which is the unimproved form
        // of the message the arm below was fixed to give.
        assert!(
            !remaining.is_zero(),
            "deadline reached: established={established}, advertised={advertised:?}. \
             Same reading as the timeout below -- both set means RETAINED."
        );
        match tokio::time::timeout(remaining, observer.select_next_some()).await {
            Ok(libp2p::swarm::SwarmEvent::ConnectionEstablished { .. }) => established = true,
            Ok(libp2p::swarm::SwarmEvent::Behaviour(libp2p::identify::Event::Received {
                info,
                ..
            })) => {
                // OBSERVED, NOT ASSERTED, and the difference was a
                // review finding on this very head. An earlier version
                // panicked here, on the strength of having measured
                // `None` five runs out of five. That assertion was wrong
                // twice over:
                //
                // - it is SCHEDULER-DEPENDENT. The emptiness rests on
                //   the subject's handler not getting CPU between
                //   `refuse.push` and `close_connection` taking effect.
                //   Under load it can, and the test would then fail with
                //   no behavioural regression at all.
                // - it pinned the OPPOSITE of an accepted contract.
                //   `transport/libp2p/CONNECTIVITY.md`'s protocol matrix
                //   gives Identify a `yes` in the infrastructure-only
                //   column, and nothing requires closing such a peer at
                //   all — that is today's behaviour, not a rule, as this
                //   test's own header says.
                //
                // So this records and does not judge. What the test
                // asserts is the establish-then-close, which comes from
                // one ordered event stream and does not depend on
                // scheduling.
                let protocols: Vec<String> =
                    info.protocols.iter().map(ToString::to_string).collect();
                eprintln!(
                    "note: subject advertised {protocols:?} before refusing an \
                     infrastructure-only peer -- recorded, not a failure; see this \
                     arm's comment"
                );
                advertised = Some(protocols);
            }
            Ok(libp2p::swarm::SwarmEvent::ConnectionClosed { .. }) => {
                // NOT an assertion that can fail. libp2p emits
                // `ConnectionClosed` only for a connection it previously
                // reported established, so this holds by construction --
                // an earlier version asserted it as though it were the
                // refusal-before-establish check, which it is not. That
                // direction surfaces as `OutgoingConnectionError` and is
                // caught by the arm below. So there is no statement here
                // at all: a `debug_assert!` would vanish under
                // `--release`, leaving a check whose presence depends on
                // the profile for a condition that holds by
                // construction.
                //
                // The observer cannot have caused this close: its own
                // idle timeout is well beyond the whole test window.
                break;
            }
            Ok(libp2p::swarm::SwarmEvent::OutgoingConnectionError { error, .. }) => {
                // THE REFUSAL-BEFORE-ESTABLISH DIRECTION. If this fires,
                // the peer was refused at or before the handshake, which
                // is what the wording this test replaced claimed already
                // happened.
                panic!("the dial failed before establishment: {error:?}");
            }
            Ok(_) => {}
            // THIS IS WHERE RETENTION LANDS, and it moved here when the
            // `Received` arm stopped panicking: that arm now records and
            // continues, so a KEPT peer produces an establish, an
            // Identify, no close, and finally this timeout. Two earlier
            // versions of this comment named the wrong arm in each
            // direction -- the failure this test most needs to report
            // clearly is the one whose diagnostic kept being wrong.
            //
            // So the message reports what was actually seen rather than
            // asserting which case it is.
            Err(_) => panic!(
                "timed out after {PATIENCE:?}: established={established}, \
                 advertised={advertised:?}. This test requires such a peer to be \
                 established and then CLOSED -- today's behaviour, not a rule any \
                 accepted document states (see this file's header); no close arrived. If `established` and \
                 `advertised` are both set, the peer was RETAINED and told those \
                 protocols -- the §14 exposure, live. That is the state the planned \
                 `ClassGated<B>` restriction is meant to make safe; it is not built \
                 yet, so see the plan's §14 rather than looking for the type. If established with nothing advertised, it was \
                 held open in silence. If not established, nothing arrived at all."
            ),
        }
    }

    subject.shutdown().await.expect("stops");
}
