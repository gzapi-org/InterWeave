// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton

//! The numbered observations. Each returns findings into a [`Report`];
//! the process exits non-zero if any required one is false, so
//! `cargo run` cannot report success while its own output disproves the
//! record.

use std::time::Duration;

use interweave_transport_api::TransportIdentity;
use interweave_transport_runtime::{ConnectionPolicy, DialDenial, DialOrigin, DialRequest};
use libp2p::Multiaddr;

use crate::node::{Node, Roles};
use crate::report::Report;
use crate::topology::{pump, pump_until};

/// A relayed listen address for `client` through `relay`.
fn circuit_addr(relay_addr: &Multiaddr, relay: &libp2p::PeerId) -> Multiaddr {
    format!("{relay_addr}/p2p/{relay}/p2p-circuit")
        .parse()
        .expect("a circuit address")
}

/// R1 — what the pinned crates actually emit.
///
/// Recorded rather than assumed: every later observation reads these
/// events, and a spike that asserted a shape the crate does not produce
/// would fail for the wrong reason.
pub async fn r1_crate_semantics(report: &mut Report) {
    let mut server = Node::new(Roles::infrastructure(), &[], &[]);
    let server_addr = server.listen().await;
    let server_id = server.identity.clone();

    // The client trusts the server for DATA PLANE here, which is the
    // control: R3 repeats it with infrastructure-only authorization and
    // the difference is the finding.
    let server_peer_for_dial = server.peer_id;
    let mut client = Node::new(Roles::client(), &[server_id], &[]);
    let _ = client.listen().await;
    client
        .dial(server_peer_for_dial, server_addr.clone())
        .expect("dial accepted");

    let server_peer = server.peer_id;
    let settled = {
        let mut nodes = [&mut client, &mut server];
        pump_until(&mut nodes, Duration::from_secs(10), |n| {
            n[0].observed.connected.contains(&server_peer)
        })
        .await
    };
    report.require(
        "R1.1",
        settled,
        "a client connects to an infrastructure node over loopback",
    );

    {
        let mut nodes = [&mut client, &mut server];
        pump(&mut nodes, Duration::from_secs(8)).await;
    }

    report.note(
        "R1.2",
        format!(
            "autonat client events: {:?}",
            client.observed.details("autonat-client")
        ),
    );
    report.note(
        "R1.3",
        format!(
            "autonat server events: {:?}",
            server.observed.details("autonat-server")
        ),
    );
    report.note(
        "R1.4",
        format!(
            "client external addresses: {:?}",
            client.observed.external_addresses
        ),
    );
    report.note(
        "R1.5",
        format!(
            "identify protocols the client saw from the server: {:?}",
            client.observed.identify_protocols.get(&server_peer)
        ),
    );

    // REQUIRED, because F7 treats this list as the baseline the Stage
    // 11 exposure correction is measured against. Noted only, a run
    // where Identify never arrived — or where the pinned crate
    // advertised something else — would exit 0 with the baseline
    // unobserved.
    let advertised = client
        .observed
        .identify_protocols
        .get(&server_peer)
        .cloned()
        .unwrap_or_default();
    report.require(
        "R1.6",
        !advertised.is_empty(),
        "Identify arrived, so the advertised protocol list was actually observed",
    );
    // THE WHOLE LIST, not a subset. Review finding on PR #69: the
    // README records F7 as an exact four-protocol set — it is the list
    // a data-plane restriction must leave intact — while this loop
    // checked two of them, so a run that dropped Identify's own
    // protocols or gained an unexpected one exited 0 while
    // contradicting the recorded evidence.
    const EXPECTED_INFRASTRUCTURE_PROTOCOLS: [&str; 4] = [
        "/ipfs/id/1.0.0",
        "/ipfs/id/push/1.0.0",
        "/libp2p/autonat/2/dial-request",
        "/libp2p/circuit/relay/0.2.0/hop",
    ];
    for expected in EXPECTED_INFRASTRUCTURE_PROTOCOLS {
        report.require(
            "R1.7",
            advertised.iter().any(|p| p == expected),
            &format!("an infrastructure node advertises {expected}"),
        );
    }
    report.require(
        "R1.8",
        advertised.len() == EXPECTED_INFRASTRUCTURE_PROTOCOLS.len()
            && advertised
                .iter()
                .all(|p| EXPECTED_INFRASTRUCTURE_PROTOCOLS.contains(&p.as_str())),
        &format!(
            "and advertises NOTHING ELSE — F7's list is exact, because it is what a \
             data-plane restriction must leave intact: {advertised:?}"
        ),
    );
}

/// R2 — every behaviour-originated dial is attributable.
///
/// THE QUESTION STAGE 11 CANNOT PROCEED WITHOUT. Production's pending
/// hook is handed a `ConnectionId`, an `Option<PeerId>` and an address
/// slice whose contents depend on the origin — empty for a Kademlia
/// query, one candidate for a relay reservation (R2.9) or an AutoNAT
/// dial-back (R4.10). None of them names the behaviour that asked, so
/// the hook inferred `KademliaQuery` at phase A's close, because
/// Kademlia was the only behaviour that could dial. With three more,
/// that inference is wrong for every one of them — and wrong in the
/// direction that fails closed against the infrastructure the stack
/// needs, because `KademliaQuery.is_data_plane()` is true. Stage 11
/// step 1 replaced the inference with the mechanism below; this
/// experiment measured the mechanism before it shipped.
///
/// The mechanism under test announces `ConnectionId -> DialOrigin` from
/// the originating behaviour's own `poll`. What this measures is
/// whether the note is ALWAYS there when the gate looks.
pub async fn r2_dial_attribution(report: &mut Report) {
    let mut relay_node = Node::new(Roles::infrastructure(), &[], &[]);
    let relay_addr = relay_node.listen().await;
    // WITHOUT THIS THE RESERVATION CARRIES NO ADDRESSES. The relay
    // server builds a reservation's address list from its own
    // `ExternalAddresses` (libp2p-relay 0.21.1 `behaviour.rs:449`), and
    // a loopback node that never calls `add_external_address` has none
    // — so the client accepts a reservation it cannot use, closes its
    // listener with `NoAddressesInReservation`, and the relay drops the
    // reservation with the connection. A later CONNECT is then answered
    // NO_RESERVATION against a reservation the relay did accept.
    relay_node.swarm.add_external_address(relay_addr.clone());
    let relay_id = relay_node.identity.clone();
    let relay_peer = relay_node.peer_id;

    let mut client = Node::new(Roles::client(), &[relay_id], &[]);
    let _ = client.listen().await;

    // NOT CONNECTED FIRST, on purpose. The first version of this
    // experiment dialled the relay manually and only then asked to
    // reserve — so the relay client had a connection already and never
    // needed to dial, and the whole run produced exactly one dial,
    // which was the harness's own MANUAL one. R2.7 passed on it and
    // measured nothing about the mechanism it exists to test.
    //
    // Listening on a circuit address for a relay we hold no connection
    // to is what forces `relay::client` to originate the dial itself.
    let circuit = circuit_addr(&relay_addr, &relay_peer);
    client
        .swarm
        .add_peer_address(relay_peer, relay_addr.clone());
    client.add_relay(relay_peer);
    let _ = client.swarm.listen_on(circuit);

    {
        let mut nodes = [&mut client, &mut relay_node];
        pump(&mut nodes, Duration::from_secs(10)).await;
    }

    let announced = client.attribution.announced();
    let resolved = client.attribution.resolved();
    let unattributed = client.attribution.unattributed();
    let behaviour_dials = client.ledger.behaviour_originated();

    report.note("R2.1", format!("announced by origin: {announced:?}"));
    report.note("R2.2", format!("resolved by origin: {resolved:?}"));
    report.note(
        "R2.3",
        format!(
            "behaviour dials seen by the gate: {behaviour_dials}, unattributed: {unattributed}"
        ),
    );
    let pending_counts = client.ledger.pending_address_counts();
    report.note(
        "R2.4",
        format!("addresses the PENDING hook was handed, per dial: {pending_counts:?}"),
    );
    // REQUIRED, because F4 is a claim ABOUT this number: SPIKE-003
    // recorded an empty list for a Kademlia dial, and a relay dial
    // carries one. Noted only, a run where it arrived with zero would
    // exit 0 while the README said otherwise — and the note's own text
    // used to say "expected all zero", contradicting the finding it
    // was evidence for.
    report.require(
        "R2.9",
        pending_counts == vec![1],
        &format!(
            "the relay reservation dial reached the pending hook carrying exactly one \
             address, unlike Kademlia's (got {pending_counts:?})"
        ),
    );
    report.note(
        "R2.5",
        format!(
            "addresses at the ESTABLISHED hook: {:?}",
            client.ledger.established_addresses()
        ),
    );

    // THE REQUIRED CLAIM. Not "some dials were attributed" — every one
    // the gate met had a note, because a single miss is a dial
    // production would misclassify.
    report.require(
        "R2.6",
        unattributed == 0,
        "every behaviour-originated dial the gate met carried an announced origin",
    );
    // NOT `> 0`: a manual dial is also "a dial". The claim is that a
    // BEHAVIOUR originated one and the mechanism named it, so the
    // assertion has to name the origin it expects.
    let relay_dials = resolved.get("relay-reservation").copied().unwrap_or(0);
    report.require(
        "R2.7",
        relay_dials > 0,
        "the relay client originated a dial of its own and the gate resolved it as \
         RelayReservation, so R2.6 is not vacuous",
    );
    report.require(
        "R2.8",
        client.attribution.outstanding() == 0,
        "no announced dial went unclaimed, so the note map does not grow without bound",
    );

    // AND THE PATH THAT ACTUALLY LEAKS. Review finding on PR #69: R2.8
    // above follows only the reservation, whose note the pending hook
    // consumes — so deleting the `DialFailure` cleanup leaves it green
    // and the fix unverified.
    //
    // A dial to an address nothing listens on is refused by the Swarm
    // and never reaches the gate, so its note can only be dropped by
    // the cleanup. `outstanding` after it is what distinguishes the
    // two.
    // A dial to a dead address is NOT that path — the swarm accepts it,
    // the gate sees it, the note is consumed normally, and a first
    // version of this observation passed with the cleanup deleted. What
    // the Swarm refuses SYNCHRONOUSLY, before any hook, is a dial whose
    // `PeerCondition` is false: the client is already connected to the
    // relay, so `Disconnected` cannot hold.
    let before = client.attribution.outstanding();
    let refused = client.dial_if_disconnected(relay_peer, relay_addr.clone());
    report.require(
        "R2.11",
        refused.is_err(),
        "the Swarm refused the dial before any hook ran, which is the path that leaks",
    );
    {
        let mut nodes = [&mut client, &mut relay_node];
        pump(&mut nodes, Duration::from_secs(5)).await;
    }
    report.require(
        "R2.10",
        client.attribution.outstanding() == before,
        &format!(
            "a dial that failed without reaching the gate left no note behind (before \
             {before}, after {})",
            client.attribution.outstanding()
        ),
    );
}

/// R3 — infrastructure authorization does not reach the data plane.
///
/// ADR-0036's whole point, exercised against the REAL policy rather
/// than a restatement of it: the same peer, the same address, the same
/// instant, admitted for reachability and refused for application
/// traffic.
pub fn r3_infrastructure_cannot_reach_the_data_plane(report: &mut Report) {
    use interweave_transport_api::TransportIdentity;
    use interweave_transport_runtime::{ConnectionManager, TrustSources};
    use interweave_trust_api::{InfrastructureSet, PeerTrustPolicy};

    let relay = TransportIdentity::parse(
        libp2p::identity::Keypair::generate_ed25519()
            .public()
            .to_peer_id()
            .to_base58(),
    )
    .expect("canonical");

    // F5, ASSERTED RATHER THAN NARRATED. Review finding on PR #69: the
    // README stated `ConnectionPolicy::default()` refuses everything
    // and nothing in the harness ever built one, so the finding had no
    // observation at all — it was the reason this experiment first
    // measured nothing, recorded as a lesson and not as a check.
    {
        let mut zeroed = ConnectionManager::new(ConnectionPolicy::default(), 32);
        let _ = zeroed.set_trust(
            TrustSources::new(
                PeerTrustPolicy::new([relay.clone()]).expect("small allowlist"),
                InfrastructureSet::default(),
            ),
            &[],
        );
        let refused = zeroed.handle().admit(
            &DialRequest {
                peer: Some(relay.clone()),
                address: String::new(),
                origin: DialOrigin::Manual,
            },
            0,
        );
        report.require(
            "R3.7",
            matches!(refused, Err(DialDenial::ConnectionLimitReached)),
            &format!(
                "`ConnectionPolicy::default()` refuses a fully TRUSTED peer with \
                 ConnectionLimitReached, because its ceilings are zero — the default is a \
                 refuse-everything policy, not a permissive one, and a fixture taking it \
                 measures nothing about class: {refused:?}"
            ),
        );
    }

    // CEILINGS, or this proves nothing. `ConnectionPolicy::default()`
    // carries `max_connections: 0`, so the first version of this
    // experiment refused all eight origins with `ConnectionLimitReached`
    // — every R3.2 "is refused" passed for a reason that had nothing to
    // do with class, and every R3.1 failed. A test that cannot tell
    // `NotAuthorizedForDataPlane` from "no room" is not testing ADR-0036.
    let mut manager = ConnectionManager::new(ConnectionPolicy::new(32, 32), 32);
    let _ = manager.set_trust(
        TrustSources::new(
            PeerTrustPolicy::new([]).expect("empty allowlist"),
            InfrastructureSet::new([relay.clone()]).expect("one relay"),
        ),
        &[],
    );
    let handle = manager.handle();

    let ask = |origin: DialOrigin| {
        handle.admit(
            &DialRequest {
                peer: Some(relay.clone()),
                address: String::new(),
                origin,
            },
            0,
        )
    };

    // THE CONTROL PROTOCOLS ADR-0036's matrix permits for this class:
    // AutoNAT probe control, and relay RESERVATION control. Both name
    // the infrastructure peer as the party the exchange is WITH, which
    // is what "eligible" in that row means.
    for origin in [DialOrigin::RelayReservation, DialOrigin::AutonatProbe] {
        let outcome = ask(origin);
        report.require(
            "R3.1",
            outcome.is_ok(),
            &format!("{origin:?} is admitted for an infrastructure-only peer"),
        );
    }

    // AND `RelayCircuit`, WHICH IS NOT ONE OF THEM — pinned in its
    // current shape rather than approved, exactly as R3.5 pins D1.
    // Review finding on PR #69: this origin sat in the loop above under
    // the heading "the matrix permits", while D2 records the same
    // admission as a violation. A spike that asserts today's behaviour
    // is CORRECT cannot find a bug in it, which is fixture bug 5, and
    // leaving it there would have failed this experiment on the commit
    // that fixed D2 — telling the implementer the matrix permits what
    // they had just correctly forbidden.
    //
    // R7.4 is where D2 is recorded, with its control. This assertion
    // exists so that R3 does not silently disagree with it.
    let circuit = ask(DialOrigin::RelayCircuit);
    report.require(
        "R3.6",
        circuit.is_ok(),
        &format!(
            "TODAY'S BEHAVIOUR, pinned and NOT endorsed: RelayCircuit is admitted for an \
             infrastructure-only peer ({circuit:?}). See D2 — a circuit names the \
             DESTINATION, so this is not the \"relay control\" row of the matrix"
        ),
    );

    // AND ONE THE MATRIX DENIES. Review finding on PR #69, and it is
    // the most consequential thing this spike found, because it is a
    // defect in THIS project rather than in a dependency.
    //
    // ADR-0036's protocol-admission matrix reads "DCUtR with that peer
    // as application destination | DataPlaneTrusted: yes |
    // ConnectivityInfrastructureOnly: **no**", and DCUTR.md §2 says
    // never to initiate DCUtR merely with an infrastructure-only peer
    // as the destination. But `DialOrigin::is_data_plane` lists only
    // Manual, ConnectionManager, DiscoveryReconnect and KademliaQuery,
    // so `DcutrHolePunch` is treated as control-plane and admitted.
    //
    // The earlier version of this experiment REQUIRED that admission,
    // which recorded the violation as evidence that the split holds.
    // It is recorded as a divergence instead, and asserted in its
    // current shape so that fixing it is noticed here.
    let dcutr = ask(DialOrigin::DcutrHolePunch);
    report.require(
        "R3.5",
        dcutr.is_ok(),
        "DcutrHolePunch is TODAY admitted for an infrastructure-only peer — asserted so \
         that changing it fails this observation rather than passing silently",
    );
    report.divergence(
        "D1",
        "DialOrigin::DcutrHolePunch is admitted for a ConnectivityInfrastructureOnly peer, \
         because DialOrigin::is_data_plane omits it and the policy therefore treats a \
         hole-punch as control-plane traffic",
        "ADR-0036 protocol-admission matrix (DCUtR with that peer as application \
         destination: no) and architecture/transport/libp2p/DCUTR.md §2. Stage 11 must \
         either add DcutrHolePunch to is_data_plane or gate it on the DESTINATION's class \
         — the two are not the same rule, since a hole-punch THROUGH infrastructure toward \
         a trusted peer is legitimate. [CORRECTED 2026-09-03: they ARE the same rule. Both \
         consumers read the predicate only in the ConnectivityInfrastructureOnly arm and \
         the class is the DIALLED peer's, so the trusted case never reaches it. That the \
         punch dial names the far end is read from the crate, not measured here.]",
    );
    for origin in [
        DialOrigin::KademliaQuery,
        DialOrigin::ConnectionManager,
        DialOrigin::Manual,
        DialOrigin::DiscoveryReconnect,
    ] {
        // THE REASON, not merely the refusal. Any misconfiguration —
        // a zero ceiling, a drain flag — refuses everything, and a
        // test that only asked `is_err()` would report ADR-0036 held
        // while measuring something else entirely.
        let denial = ask(origin).err();
        report.require(
            "R3.2",
            denial == Some(DialDenial::NotAuthorizedForDataPlane),
            &format!(
                "{origin:?} is refused for an infrastructure-only peer AS a data-plane \
                 origin (got {denial:?})"
            ),
        );
    }

    // AND THE PRECEDENCE, EXERCISED. ADR-0036 says data-plane trust
    // wins when a peer is in both sets, and the README says this run
    // observed it — which was true only of a mutation I ran by hand.
    // A regression in that path would have left every assertion above
    // passing. So the same peer is added to the data-plane allowlist
    // and asked again.
    let mut both = ConnectionManager::new(ConnectionPolicy::new(32, 32), 32);
    let _ = both.set_trust(
        TrustSources::new(
            PeerTrustPolicy::new([relay.clone()]).expect("one peer"),
            InfrastructureSet::new([relay.clone()]).expect("the same peer"),
        ),
        &[],
    );
    let both_handle = both.handle();
    for origin in [
        DialOrigin::KademliaQuery,
        DialOrigin::ConnectionManager,
        DialOrigin::Manual,
        DialOrigin::DiscoveryReconnect,
    ] {
        let admitted = both_handle
            .admit(
                &DialRequest {
                    peer: Some(relay.clone()),
                    address: String::new(),
                    origin,
                },
                0,
            )
            .is_ok();
        report.require(
            "R3.4",
            admitted,
            &format!("{origin:?} is ADMITTED for a peer in both sets — data-plane trust wins"),
        );
    }

    // THE CONSEQUENCE FOR STAGE 11, stated as an observation rather
    // than left implicit: an unattributed dial defaults to
    // `KademliaQuery` in production today, and R3.2 shows that is
    // refused for exactly the peers the reachability stack must dial.
    // So attribution (R2) is not a nicety; without it the stack fails
    // closed against its own infrastructure.
    report.note(
        "R3.3",
        "an unattributed dial would be admitted as KademliaQuery, which R3.2 shows is refused \
         for an infrastructure-only peer — so R2's mechanism is load-bearing, not cosmetic"
            .to_owned(),
    );
}

/// R4 — what the AutoNAT v2 server validates before dialling back.
///
/// `AUTONAT.md` §7 makes four checks mandatory: literal IP only, the
/// candidate IP equal to the observed source IP, prohibited address
/// classes refused, and a mismatch treated as a probe failure rather
/// than a generic dial. This records what the pinned crate does, which
/// is what decides whether Stage 11 implements those checks itself.
pub async fn r4_autonat_server_dial_back(report: &mut Report) {
    // Read from the crate rather than inferred: the server's request
    // handler pops the LAST supplied address, and when it differs from
    // the observed address it charges the client "dial data" and then
    // dials it anyway. There is no IP-class filter and no
    // equality-with-source requirement in that path.
    report.note(
        "R4.1",
        "libp2p-autonat 0.15.0 v2 server: handle_request_internal pops addrs.last(), and on \
         `addr != observed_multiaddr` requests dial-data (amortization) rather than refusing; \
         no literal-IP, source-equality or special-use class check exists in that path"
            .to_owned(),
    );
    report.note(
        "R4.2",
        "the dial-back is an ordinary ToSwarm::Dial with PeerCondition::Always and \
         allocate_new_port (v2/server/behaviour.rs), so it DOES traverse the root gate's \
         pending and established hooks — which is where the missing checks can be added"
            .to_owned(),
    );

    // And the consequence, MEASURED — in two runs, not one.
    //
    // The first version connected the client to BOTH servers and hoped
    // the probe went to the permissive one. The AutoNAT client picks
    // among the servers it is connected to, so which one was probed
    // varied run to run: the recorded evidence came from a lucky pass,
    // and a later run had the permissive server make zero dials while
    // the strict one refused the only probe. Review finding on PR #69,
    // and the requirements below are what turn that from a note nobody
    // checks into a failure.
    //
    // One server per scenario is what makes each deterministic.

    // SCENARIO A — a server that trusts the client. Its dial-back must
    // reach the root gate and be admitted there as AutonatProbe, which
    // is precisely what makes F2 fixable at the gate rather than
    // blocking on a crate change.
    let mut client_a = Node::new(Roles::client(), &[], &[]);
    let client_a_id = client_a.identity.clone();
    let mut permissive = Node::new(Roles::infrastructure(), &[client_a_id], &[]);
    let permissive_addr = permissive.listen().await;
    let permissive_peer = permissive.peer_id;
    client_a.trust_data_plane(&[permissive.identity.clone()]);
    let _ = client_a.listen().await;
    client_a
        .dial(permissive_peer, permissive_addr)
        .expect("dial accepted");

    {
        let mut nodes = [&mut client_a, &mut permissive];
        pump_until(&mut nodes, Duration::from_secs(20), |n| {
            n[1].ledger
                .allowed_by_origin()
                .get("autonat-probe")
                .copied()
                .unwrap_or(0)
                > 0
        })
        .await;
    }

    // SCENARIO B — a server that trusts nobody. The crate is equally
    // willing to dial back; its own gate is what refuses.
    let mut client_b = Node::new(Roles::client(), &[], &[]);
    let mut strict = Node::new(Roles::infrastructure(), &[], &[]);
    let strict_addr = strict.listen().await;
    let strict_peer = strict.peer_id;
    client_b.trust_data_plane(&[strict.identity.clone()]);
    let _ = client_b.listen().await;
    client_b
        .dial(strict_peer, strict_addr)
        .expect("dial accepted");

    {
        let mut nodes = [&mut client_b, &mut strict];
        pump_until(&mut nodes, Duration::from_secs(20), |n| {
            n[1].ledger.behaviour_originated() > 0
        })
        .await;
    }

    let permissive_probes = permissive
        .ledger
        .allowed_by_origin()
        .get("autonat-probe")
        .copied()
        .unwrap_or(0);
    report.require(
        "R4.6",
        permissive_probes > 0,
        "the AutoNAT server's dial-back reached the root gate and was admitted there as \
         AutonatProbe — which is what makes F2 fixable at the gate",
    );
    report.require(
        "R4.7",
        client_a
            .observed
            .details("autonat-client")
            .iter()
            .any(|d| d.contains("result=Ok(())")),
        "the probe completed, so the dial-back was real work rather than a refusal counted \
         as one",
    );
    // THE CONTROL. Without it R4.6 would pass for a gate that admitted
    // everything, and the claim that the gate is where the §7 checks
    // belong would rest on nothing.
    report.require(
        "R4.8",
        strict.ledger.behaviour_originated() > 0
            && strict.ledger.allowed_by_origin().is_empty()
            && strict
                .ledger
                .refusals()
                .keys()
                .any(|r| r.contains("Unauthorized")),
        &format!(
            "an untrusting server's dial-back is made by the crate and REFUSED by its own \
             root gate ON TRUST — `allowed.is_empty()` alone would also hold for a dial \
             refused as unattributed or nameless, which is a different claim: {:?}",
            strict.ledger.refusals()
        ),
    );

    report.note(
        "R4.3",
        format!(
            "strict server gate ledger: behaviour dials={}, allowed={:?}, refusals={:?}",
            strict.ledger.behaviour_originated(),
            strict.ledger.allowed_by_origin(),
            strict.ledger.refusals()
        ),
    );
    report.note(
        "R4.4",
        format!(
            "permissive server gate ledger: behaviour dials={}, allowed={:?}, refusals={:?}",
            permissive.ledger.behaviour_originated(),
            permissive.ledger.allowed_by_origin(),
            permissive.ledger.refusals()
        ),
    );
    report.note(
        "R4.5",
        format!(
            "client autonat results: {:?}",
            client_a.observed.details("autonat-client")
        ),
    );

    // WHERE THE §7 CHECKS CAN ACTUALLY RUN. Review finding on PR #69,
    // against F2's own recommendation: `handle_established_outbound_
    // connection` runs AFTER the TCP connection is open, so a server
    // that validated there would already have contacted the target —
    // which is the whole of what an SSRF check exists to prevent.
    //
    // So the question is whether the dial-back's address is present at
    // the PENDING hook, before any socket. Measured rather than
    // assumed, because F4 showed the answer differs per behaviour.
    let server_pending = permissive.ledger.pending_address_counts();
    report.note(
        "R4.9",
        format!("addresses at the AutoNAT server's PENDING hook, per dial: {server_pending:?}"),
    );
    report.require(
        "R4.10",
        !server_pending.is_empty() && server_pending.iter().all(|n| *n == 1),
        &format!(
            "the dial-back candidate is present at the pending hook, BEFORE any socket \
             opens — so an address check there precedes contact, which one at the \
             established hook would not (got {server_pending:?})"
        ),
    );

    // F2 MEASURED, not read. Review finding on PR #69: F2's central
    // claim — that the crate does not implement §7 — rested on R4.1, a
    // NOTE, so a patch release adding the check would leave this
    // experiment green while the record said otherwise.
    //
    // §7: "loopback, unspecified, multicast, link-local, RFC1918
    // private IPv4, IPv6 ULA, and other non-global/special-use
    // destinations are rejected ... even if supplied by an authorized
    // peer", and "mismatch/rejection is a probe failure and never
    // becomes a generic dial request". Every candidate in this run is
    // `127.0.0.1` — squarely inside that list — and the server emitted
    // the dial anyway. That is the missing check, observed.
    //
    // What this does NOT reach is the source-equality rule: on loopback
    // the candidate and the observed source are the same address, so
    // only the special-use half is measured. The unrelated-public-IP
    // case §7 asks conformance to attempt needs a second interface and
    // is phase B.
    let candidates: Vec<String> = permissive
        .ledger
        .pending_addresses()
        .into_iter()
        .flatten()
        .collect();
    report.note(
        "R4.11",
        format!("dial-back candidates admitted: {candidates:?}"),
    );
    report.require(
        "R4.12",
        !candidates.is_empty() && candidates.iter().all(|a| a.contains("/ip4/127.")),
        &format!(
            "the server dialled back to a LOOPBACK candidate, which AUTONAT.md §7 requires \
             be rejected even from an authorized peer — so the crate's missing dial-back \
             restriction is measured rather than read: {candidates:?}"
        ),
    );
}

/// R5 — a relay CIRCUIT is a different origin from a reservation, and
/// the attribution mechanism has to be able to say so.
///
/// Review finding on PR #69: giving each wrapper one fixed origin meant
/// `relay::client::Behaviour` announced every dial as
/// `RelayReservation`, so `RelayCircuit` — a distinct variant the
/// production policy already defines — could never reach the gate.
/// R2 exercises only a reservation and could not have revealed it.
///
/// What this measures is both halves: that a circuit toward a
/// destination is classified apart from a reservation to the relay,
/// and — if the relay TRANSPORT rather than the behaviour originates
/// the connection — that the mechanism's limit is recorded rather than
/// assumed away.
pub async fn r5_circuit_is_not_a_reservation(report: &mut Report) {
    let mut relay_node = Node::new(Roles::infrastructure(), &[], &[]);
    let relay_addr = relay_node.listen().await;
    // WITHOUT THIS THE RESERVATION CARRIES NO ADDRESSES. The relay
    // server builds a reservation's address list from its own
    // `ExternalAddresses` (libp2p-relay 0.21.1 `behaviour.rs:449`), and
    // a loopback node that never calls `add_external_address` has none
    // — so the client accepts a reservation it cannot use, closes its
    // listener with `NoAddressesInReservation`, and the relay drops the
    // reservation with the connection. A later CONNECT is then answered
    // NO_RESERVATION against a reservation the relay did accept.
    relay_node.swarm.add_external_address(relay_addr.clone());
    let relay_id = relay_node.identity.clone();
    let relay_peer = relay_node.peer_id;

    // The destination reserves a slot on the relay and listens on it.
    let mut dest = Node::new(Roles::client(), std::slice::from_ref(&relay_id), &[]);
    let _ = dest.listen().await;
    dest.add_relay(relay_peer);
    dest.swarm.add_peer_address(relay_peer, relay_addr.clone());
    let dest_peer = dest.peer_id;
    let dest_id = dest.identity.clone();
    let circuit = circuit_addr(&relay_addr, &relay_peer);
    let _ = dest.swarm.listen_on(circuit.clone());

    // The source trusts both, knows the relay, and dials the
    // destination THROUGH it.
    let mut source = Node::new(Roles::client(), &[relay_id, dest_id], &[]);
    let _ = source.listen().await;
    source.add_relay(relay_peer);
    source
        .swarm
        .add_peer_address(relay_peer, relay_addr.clone());

    // WAIT ON THE RELAY, not on the destination. The relay is where a
    // reservation exists; the client's own event can lag it, and the
    // first version dialled the circuit before the relay had the
    // reservation — the relay answered NO_RESERVATION and the run
    // reported a failure that was a race in the fixture.
    {
        let mut nodes = [&mut dest, &mut relay_node, &mut source];
        pump_until(&mut nodes, Duration::from_secs(20), |n| {
            n[1].observed
                .details("relay-server")
                .iter()
                .any(|d| d.contains("ReservationReqAccepted"))
        })
        .await;
    }
    let reserved = relay_node
        .observed
        .details("relay-server")
        .iter()
        .any(|d| d.contains("ReservationReqAccepted"));
    report.require(
        "R5.1",
        reserved,
        "the destination obtained a relay reservation, so a circuit toward it is possible",
    );

    let via_relay: Multiaddr = format!("{circuit}/p2p/{dest_peer}")
        .parse()
        .expect("a circuit address naming the destination");
    // The result is kept: a synchronous refusal here would make every
    // claim below vacuous, and R5.8 is what notices.
    let circuit_dial = source.dial(dest_peer, via_relay);
    report.require(
        "R5.9",
        circuit_dial.is_ok(),
        "the circuit dial was accepted by the swarm rather than refused synchronously",
    );

    // WAIT FOR THE OUTCOME, not for a budget. R5.7 and R5.12 assert
    // that the circuit was accepted and established, so pumping a fixed
    // twelve seconds makes those assertions a statement about this
    // machine's load rather than about the crate — and an occasional
    // failure is then indistinguishable from a real one.
    {
        let mut nodes = [&mut dest, &mut relay_node, &mut source];
        pump_until(&mut nodes, Duration::from_secs(30), |n| {
            n[1].observed
                .details("relay-server")
                .iter()
                .any(|d| d.contains("CircuitReqAccepted"))
                && n[2]
                    .observed
                    .details("relay-circuit-outbound")
                    .iter()
                    .any(|d| d.contains("OutboundCircuitEstablished"))
        })
        .await;
    }

    let resolved = source.attribution.resolved();
    report.note("R5.2", format!("source resolved origins: {resolved:?}"));
    report.note(
        "R5.3",
        format!(
            "source connected: {:?}, relay server events: {:?}",
            source.observed.connected.len(),
            relay_node.observed.details("relay-server")
        ),
    );
    report.note(
        "R5.4",
        format!(
            "source relay-client events: {:?}",
            source.observed.details("relay-circuit-outbound")
        ),
    );

    // THE MECHANISM'S CAPABILITY, asserted where it can be: the
    // classifier answers RelayCircuit for a peer that is not a
    // configured relay. Whether the relay BEHAVIOUR originates such a
    // dial (rather than the relay transport) is what R5.2 records —
    // and if it does not, the note says so rather than the requirement
    // pretending it did.
    report.require(
        "R5.5",
        source.attribution.unattributed() == 0,
        "every dial the source's gate met was still attributable once circuits are in play",
    );

    // WHAT R5 ACTUALLY SETTLED, and it is not what the review or I
    // expected. The source's circuit dial resolved as `manual`, not as
    // `relay-circuit`: dialling `/…/p2p-circuit/p2p/<dest>` is handled
    // by the relay TRANSPORT, so `relay::client::Behaviour` emits no
    // `ToSwarm::Dial` for it and the poll-time wrapper never sees one.
    //
    // So `RelayCircuit` is not a behaviour-originated origin at all —
    // it is a COMMAND-PATH one. The caller dialling a peer through a
    // relay knows it is doing so, and production's `GatedSwarm::dial`
    // is where that origin gets set, from the address it was handed.
    // The classifier still matters for the reservation/circuit split
    // IF a future crate version dials circuits from the behaviour; it
    // is not what produces `RelayCircuit` today.
    // THE DIAL MUST HAVE HAPPENED for the negative to mean anything.
    // If `source.dial` were refused synchronously, `resolved` would be
    // empty and R5.6 would pass having observed no circuit dial at all
    // — concluding the transport owns circuit dials from silence.
    report.require(
        "R5.8",
        resolved.get("manual").copied().unwrap_or(0) > 0,
        "the circuit dial reached the gate and was attributed, so R5.6's negative is a \
         measurement rather than an absence",
    );
    // AND NOT MERELY "no dial was labelled a circuit". Review finding
    // on PR #69: if the behaviour DID emit the circuit dial and the
    // classifier regressed to calling it a reservation, `resolved`
    // would hold `manual` and `relay-reservation` and every assertion
    // here would still pass while the conclusion was false. The
    // discriminator is WHO was dialled: a reservation goes to the
    // relay, a circuit toward the destination.
    let reservation_targets = source
        .attribution
        .targets(interweave_transport_runtime::DialOrigin::RelayReservation);
    report.note(
        "R5.10",
        format!(
            "relay-behaviour dials targeted: {:?} (relay={relay_peer}, dest={dest_peer})",
            reservation_targets
        ),
    );
    report.require(
        "R5.11",
        !reservation_targets.contains(&dest_peer),
        "no dial the relay BEHAVIOUR made was aimed at the destination — so the circuit \
         was not emitted by the behaviour under a regressed label",
    );
    report.require(
        "R5.6",
        !resolved.contains_key("relay-circuit"),
        "across a circuit that opened, established and carried the relayed path, the relay \
         BEHAVIOUR originated no dial — the transport did — so RelayCircuit is a \
         command-path origin",
    );
    // AND THE CIRCUIT COMPLETED, which is what lets R5.6 speak about
    // the lifecycle rather than only about the opening dial. An
    // earlier run stopped at NO_RESERVATION and this note said F3 was
    // scoped to the dial alone; the refusal was a fixture bug (the
    // relay had no external address, so its reservation carried no
    // addresses — see R5.13) and not a limit of the evidence.
    report.require(
        "R5.7",
        relay_node
            .observed
            .details("relay-server")
            .iter()
            .any(|d| d.contains("CircuitReqAccepted")),
        "the relay ACCEPTED the circuit, so what follows is observed on a live relayed \
         path rather than on a refused one",
    );
    report.require(
        "R5.12",
        source
            .observed
            .details("relay-circuit-outbound")
            .iter()
            .any(|d| d.contains("OutboundCircuitEstablished")),
        "the source established the outbound circuit, so R5.6's negative covers the whole \
         path and not only the dial that opens it",
    );
    // THE RELAY'S OWN LIMITS, recorded because RELAY.md §8 sets
    // ours and a reader should be able to compare them with what the
    // crate's defaults actually offer.
    report.note(
        "R5.13",
        format!(
            "relay-client circuit events (the limits the relay imposed): {:?}",
            source.observed.details("relay-circuit-outbound")
        ),
    );
}

/// R6 — the SHIPPED gate, refusing the reservation.
///
/// Every other experiment runs `InstrumentedGate`, which is the gate
/// Stage 11 would have to build. Measuring a proposal proves the
/// proposal works. This one puts `OutboundAdmission` — production, by
/// path, unmodified — in front of a real `relay::client::Behaviour`
/// and records what it does.
///
/// F1 is otherwise a chain of reading: the pending hook builds its
/// `DialRequest` with `origin: DialOrigin::KademliaQuery`,
/// `is_data_plane()` is true for it, and `ConnectionPolicy::admit`
/// refuses a data-plane origin for a `ConnectivityInfrastructureOnly`
/// peer. Three files. This runs the chain instead of arguing it.
///
/// # Why it is measured at the gate and not at the Swarm
///
/// The first version of this experiment watched `SwarmEvent` and saw
/// nothing at all — no `Dialing`, no `OutgoingConnectionError` — and
/// its control saw nothing either, so it could only be recorded as a
/// fact about the fixture. It was not. `Swarm::dial` denies the dial,
/// hands the behaviour `FromSwarm::DialFailure`, and returns `Err`;
/// the caller for a behaviour-emitted dial is
/// `if let Ok(()) = self.dial(opts)` (libp2p-swarm 0.47.1
/// `lib.rs:1098`), which DISCARDS it. A policy refusal of a
/// behaviour-originated dial is therefore invisible in the Swarm event
/// stream, which is finding F8 and is asserted below in its own right.
///
/// So the instrument moved to where the decision is made:
/// [`crate::production::Observing`] forwards every call to the real
/// gate and records the verdict the real gate returned.
pub async fn r6_production_gate_refuses_the_reservation(report: &mut Report) {
    use crate::production::ProductionNode;

    let mut relay_node = Node::new(Roles::infrastructure(), &[], &[]);
    let relay_addr = relay_node.listen().await;
    // WITHOUT THIS THE RESERVATION CARRIES NO ADDRESSES. The relay
    // server builds a reservation's address list from its own
    // `ExternalAddresses` (libp2p-relay 0.21.1 `behaviour.rs:449`), and
    // a loopback node that never calls `add_external_address` has none
    // — so the client accepts a reservation it cannot use, closes its
    // listener with `NoAddressesInReservation`, and the relay drops the
    // reservation with the connection. A later CONNECT is then answered
    // NO_RESERVATION against a reservation the relay did accept.
    relay_node.swarm.add_external_address(relay_addr.clone());
    let relay_id = relay_node.identity.clone();
    let relay_peer = relay_node.peer_id;

    // THE SUBJECT: the relay is authorized for reachability and
    // nothing else, which is what a relay is under ADR-0036.
    let mut node = ProductionNode::new(&[relay_id]);
    // THE CONTROL, varying exactly ONE thing. Review finding on PR
    // #69: the earlier version moved the relay from the infrastructure
    // set INTO the data-plane allowlist, which is two changes, while
    // the prose said "change nothing else". The relay stays
    // infrastructure here and data-plane trust is ADDED beside it, so
    // the single difference is whether the destination is also a
    // data-plane peer. R3.4 is why that resolves the way it does:
    // data-plane trust wins when a peer is in both sets.
    let mut control = ProductionNode::with_trust(
        std::slice::from_ref(&relay_node.identity),
        std::slice::from_ref(&relay_node.identity),
    );

    // THE THIRD NODE, and Stage 11 step 1 is why it exists. R6 used to
    // measure the gate refusing this reservation dial because the
    // pending hook called every unticketed behaviour dial
    // `KademliaQuery`. Step 1 replaced that assumption with real
    // attribution, so the dial is now admitted -- which is the fix
    // working, and which leaves F8's "a refused behaviour dial is
    // invisible" with no refusal to observe. This node announces a
    // data-plane origin for a reservation dial, which is a lie to the
    // gate and produces exactly the refusal step 1 removed. F8 is a
    // finding about the SWARM discarding a denial, so any refused
    // behaviour dial demonstrates it.
    let mut misattributed = ProductionNode::with_trust_and_origin(
        &[],
        std::slice::from_ref(&relay_node.identity),
        DialOrigin::KademliaQuery,
    );

    for target in [&mut node, &mut control, &mut misattributed] {
        let _ = target.listen().await;
        target
            .swarm
            .add_peer_address(relay_peer, relay_addr.clone());
        let circuit: Multiaddr = format!("{relay_addr}/p2p/{relay_peer}/p2p-circuit")
            .parse()
            .expect("a circuit address");
        let listen = target.swarm.listen_on(circuit);
        assert!(
            listen.is_ok(),
            "the relay transport took the listen: {listen:?}"
        );
    }

    // Drive all three for long enough that a reservation would have
    // been made had the dial been admitted.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while tokio::time::Instant::now() < deadline {
        let _ = tokio::time::timeout(
            Duration::from_millis(50),
            futures::future::poll_fn(|cx| {
                node.drain(cx);
                control.drain(cx);
                misattributed.drain(cx);
                let mut progressed = false;
                while let std::task::Poll::Ready(Some(_)) =
                    futures::StreamExt::poll_next_unpin(&mut relay_node.swarm, cx)
                {
                    progressed = true;
                }
                if progressed {
                    std::task::Poll::Ready(())
                } else {
                    std::task::Poll::Pending
                }
            }),
        )
        .await;
    }

    let subject = node.decisions.all();
    let control_decisions = control.decisions.all();
    report.note(
        "R6.1",
        format!(
            "production gate decisions, infrastructure-only relay: {:?}",
            subject
                .iter()
                .map(|d| (d.connection_id, d.peer, d.refusal.clone()))
                .collect::<Vec<_>>()
        ),
    );
    report.note(
        "R6.2",
        format!(
            "production gate decisions, control (relay data-plane trusted): {:?}",
            control_decisions
                .iter()
                .map(|d| (d.connection_id, d.peer, d.refusal.clone()))
                .collect::<Vec<_>>()
        ),
    );
    report.note(
        "R6.3",
        format!(
            "swarm-visible: subject dialing={} connected={} failures={:?}; \
             control dialing={} connected={} failures={:?}",
            node.dialing,
            node.connected,
            node.failures,
            control.dialing,
            control.connected,
            control.failures
        ),
    );

    // THE FIXTURE DIALS. Without this every claim below is vacuous:
    // a relay client that never asked to dial would produce the same
    // "no connection" outcome for reasons having nothing to do with
    // the gate. This is the assertion the first version of R6 lacked,
    // and lacking it is why it misread its own result.
    report.require(
        "R6.4",
        !subject.is_empty(),
        &format!(
            "the relay client originated a dial and the production gate was asked about it \
             ({} decision(s))",
            subject.len()
        ),
    );

    // WHAT F1 MEASURED, AND WHAT STAGE 11 STEP 1 CHANGED. F1 recorded
    // the shipped gate refusing this reservation dial because the
    // pending hook called every unticketed behaviour dial
    // `KademliaQuery`, whose `is_data_plane()` is true. Step 1 gave the
    // hook real attribution, so the same dial now arrives as
    // `RelayReservation` and is ADMITTED — a reservation is the
    // reachability purpose, not the data plane. R6.5 asserts the fix
    // rather than the defect; the original finding is F1's, and it is
    // history rather than a live divergence.
    report.require(
        "R6.5",
        !subject.is_empty() && subject.iter().all(|d| d.refusal.is_none()),
        &format!(
            "the reservation dial toward an infrastructure-only relay is ADMITTED now \
             that it carries its own origin ({} decision(s), refusals {:?})",
            subject.len(),
            node.decisions.refusals()
        ),
    );
    // AND THE CLASS CHECK STILL RUNS, which is what stops R6.5 reading
    // as "the gate stopped looking". The same dial toward the same
    // relay, announced under a data-plane origin, is refused for the
    // CLASS — so R6.5 is the ORIGIN being right, not the destination
    // going unchecked. `kademlia` alone would match every denial the
    // gate renders, which is how a PolicySuperseded run once read as a
    // class refusal, so both halves are required.
    //
    // The needle is `KademliaQuery` rather than `kademlia` because step
    // 1 renders the origin with `{origin:?}`. Matching the rendering is
    // the point -- this assertion is about WHICH origin the gate named,
    // and a rendering that stops naming it should fail here.
    report.require(
        "R6.6",
        !misattributed.decisions.refusals().is_empty()
            && misattributed
                .decisions
                .refusals()
                .iter()
                .all(|r| r.contains("KademliaQuery") && r.contains("NotAuthorizedForDataPlane")),
        &format!(
            "a reservation dial announced under a DATA-PLANE origin toward the same \
             infrastructure-only relay is refused, and the refusal names the origin AND \
             the class: {:?}",
            misattributed.decisions.refusals()
        ),
    );

    // THE CONTROL, and it is what makes R6.5 mean anything. One change
    // — the relay moved from the infrastructure set to the data-plane
    // allowlist — and the same dial is admitted. So the refusal is the
    // CLASS, not the network, the addresses or the fixture.
    report.require(
        "R6.7",
        control.decisions.admissions() > 0,
        &format!(
            "control: the same dial is ADMITTED when the relay is data-plane trusted \
             ({} admission(s), refusals {:?})",
            control.decisions.admissions(),
            control.decisions.refusals()
        ),
    );
    report.require(
        "R6.8",
        node.connected > misattributed.connected,
        &format!(
            "the attributed subject reaches the relay and the misattributed node does \
             not ({} vs {} connection(s)); the control reaches it too ({})",
            node.connected, misattributed.connected, control.connected
        ),
    );

    // F8 — THE REFUSAL IS SILENT. Not a divergence from an accepted
    // document (no document promises otherwise) but a finding that
    // binds Phase 1: the gate's own denial produces no `Dialing` and
    // no `OutgoingConnectionError`, because the Swarm discards the
    // `Err` from a behaviour-originated dial. The only outward trace
    // is the relay client giving up on its listener — reported as a
    // NORMAL close.
    report.require(
        "R6.9",
        misattributed.dialing == 0 && misattributed.outgoing_errors == 0,
        &format!(
            "a gate refusal of a behaviour dial emits no Dialing and no \
             OutgoingConnectionError — counted by EVENT VARIANT, not matched against a \
             rendering that could change (dialing {}, outgoing errors {}, swarm events \
             {:?})",
            misattributed.dialing, misattributed.outgoing_errors, misattributed.failures
        ),
    );
    // THE POSITIVE CONTROL FOR F8, without which R6.9 is an assertion
    // that nothing happened in a fixture where nothing happens. The
    // control's dial is admitted and the Swarm reports it: one
    // `Dialing`, one `ConnectionEstablished`. So the subject's silence
    // is the refusal being invisible, not this node being unable to
    // report a dial.
    report.require(
        "R6.11",
        node.dialing >= 1 && misattributed.dialing == 0,
        &format!(
            "an ADMITTED behaviour dial is visible as `Dialing` and a REFUSED one is not (admitted {} vs refused {})",
            node.dialing, misattributed.dialing
        ),
    );
    report.require(
        "R6.10",
        misattributed
            .failures
            .iter()
            .any(|f| f.contains("listener closed: Ok")),
        &format!(
            "the only outward trace of the refusal is the relay listener closing \
             SUCCESSFULLY: {:?}",
            misattributed.failures
        ),
    );
}

/// R7 — a relayed circuit is a data-plane path, and the gate does not
/// treat it as one.
///
/// ADR-0036's enforcement clause: "The root dial gate evaluates both
/// requested dial purpose and destination class. It must not authorize
/// a generic application dial merely because the PeerId is an
/// infrastructure peer." Dialling a peer through a relay is a generic
/// application dial — a circuit exists to carry application traffic —
/// and R5 established that the origin can only be set by the caller,
/// because the relay transport rather than the behaviour makes the
/// dial.
///
/// So the question this asks is narrow and answerable: with the relay
/// held as infrastructure, what does the shipped policy do with a
/// `RelayCircuit` dial toward a destination in each of the three
/// classes?
///
/// It also measures the positive half of the same clause — that the
/// authenticated END identity is what the relayed connection carries,
/// independent of the relay's — because a rule about the end PeerId is
/// worth nothing if the end PeerId is not what arrives.
pub async fn r7_relayed_path_trust(report: &mut Report) {
    // THE POLICY QUESTION FIRST, decided against the production policy
    // directly. Three destinations differing only in class, one origin,
    // one relay held as infrastructure throughout.
    let relay = TransportIdentity::parse(
        libp2p::PeerId::from_public_key(&libp2p::identity::Keypair::generate_ed25519().public())
            .to_base58(),
    )
    .expect("canonical");
    let trusted_dest = TransportIdentity::parse(
        libp2p::PeerId::from_public_key(&libp2p::identity::Keypair::generate_ed25519().public())
            .to_base58(),
    )
    .expect("canonical");
    let infra_dest = TransportIdentity::parse(
        libp2p::PeerId::from_public_key(&libp2p::identity::Keypair::generate_ed25519().public())
            .to_base58(),
    )
    .expect("canonical");
    let stranger = TransportIdentity::parse(
        libp2p::PeerId::from_public_key(&libp2p::identity::Keypair::generate_ed25519().public())
            .to_base58(),
    )
    .expect("canonical");

    let mut manager =
        interweave_transport_runtime::ConnectionManager::new(ConnectionPolicy::new(32, 32), 32);
    let _ = manager.set_trust(
        interweave_transport_runtime::TrustSources::new(
            interweave_trust_api::PeerTrustPolicy::new([trusted_dest.clone()]).expect("small"),
            interweave_trust_api::InfrastructureSet::new([relay.clone(), infra_dest.clone()])
                .expect("small"),
        ),
        &[],
    );
    let handle = manager.handle();
    let ask = |peer: &TransportIdentity, origin: DialOrigin| {
        handle.admit(
            &DialRequest {
                peer: Some(peer.clone()),
                address: "/ip4/127.0.0.1/tcp/1".to_owned(),
                origin,
            },
            0,
        )
    };

    report.require(
        "R7.1",
        ask(&trusted_dest, DialOrigin::RelayCircuit).is_ok(),
        "a circuit toward a data-plane trusted destination is admitted, which is the case \
         the feature exists for",
    );
    report.require(
        "R7.2",
        matches!(
            ask(&stranger, DialOrigin::RelayCircuit),
            Err(DialDenial::Unauthorized)
        ),
        "a circuit toward a peer in NEITHER set is refused as Unauthorized, so the class \
         check runs on the destination rather than on the relay",
    );

    // THE DIVERGENCE. `RelayCircuit` is absent from
    // `DialOrigin::is_data_plane`, so an infrastructure-only
    // destination — a peer authorized for reachability and nothing
    // else — is dialable as a circuit. A circuit is application
    // traffic by construction.
    let infra_circuit = ask(&infra_dest, DialOrigin::RelayCircuit);
    report.note(
        "R7.3",
        format!("circuit toward an infrastructure-only destination: {infra_circuit:?}"),
    );
    report.require(
        "R7.4",
        infra_circuit.is_ok(),
        "TODAY'S BEHAVIOUR, pinned so a fix fails here rather than passing silently: a \
         RelayCircuit dial toward an infrastructure-only destination is ADMITTED",
    );
    if infra_circuit.is_ok() {
        report.divergence(
            "D2",
            "DialOrigin::RelayCircuit is admitted for a ConnectivityInfrastructureOnly \
             destination, because is_data_plane omits it — so a peer authorized for \
             reachability alone is reachable over a relayed circuit, which carries \
             application traffic by construction",
            "ADR-0036 enforcement (\"the root dial gate evaluates both requested dial \
             purpose and destination class; it must not authorize a generic application \
             dial merely because the PeerId is an infrastructure peer\"). The fix is not \
             the same as D1's: RelayReservation must stay non-data-plane, since a \
             reservation IS the reachability purpose, while RelayCircuit names the \
             destination and not the relay — R5.6 is why, and it is the two origins' \
             whole difference",
        );
    }
    // AND THE CONTROL FOR THE CLAIM THAT THIS IS ABOUT THE ORIGIN, not
    // about the class check being broken: the same destination, the
    // same policy, a data-plane origin — refused.
    report.require(
        "R7.5",
        matches!(
            ask(&infra_dest, DialOrigin::Manual),
            Err(DialDenial::NotAuthorizedForDataPlane)
        ),
        "the same infrastructure-only destination IS refused under a data-plane origin, so \
         R7.4 is the origin's classification and not a broken class check",
    );

    // THE POSITIVE HALF: the end identity survives the relayed path.
    let mut relay_node = Node::new(Roles::infrastructure(), &[], &[]);
    let relay_addr = relay_node.listen().await;
    relay_node.swarm.add_external_address(relay_addr.clone());
    let relay_id = relay_node.identity.clone();
    let relay_peer = relay_node.peer_id;

    let mut dest = Node::new(Roles::client(), std::slice::from_ref(&relay_id), &[]);
    let _ = dest.listen().await;
    dest.add_relay(relay_peer);
    dest.swarm.add_peer_address(relay_peer, relay_addr.clone());
    let dest_peer = dest.peer_id;
    let dest_id = dest.identity.clone();
    let circuit = circuit_addr(&relay_addr, &relay_peer);
    let _ = dest.swarm.listen_on(circuit.clone());

    // The source holds the relay as INFRASTRUCTURE and the destination
    // in the data plane, which is the deployment ADR-0036 describes.
    let mut source = Node::new(Roles::client(), &[], &[]);
    source.set_trust_sets(&[dest_id], &[relay_id]);
    let _ = source.listen().await;
    source.add_relay(relay_peer);
    source
        .swarm
        .add_peer_address(relay_peer, relay_addr.clone());

    {
        let mut nodes = [&mut dest, &mut relay_node, &mut source];
        pump_until(&mut nodes, Duration::from_secs(20), |n| {
            n[1].observed
                .details("relay-server")
                .iter()
                .any(|d| d.contains("ReservationReqAccepted"))
        })
        .await;
    }

    let via_relay: Multiaddr = format!("{circuit}/p2p/{dest_peer}")
        .parse()
        .expect("a circuit address naming the destination");
    let dialed = source.dial_circuit(dest_peer, via_relay);
    report.require(
        "R7.6",
        dialed.is_ok(),
        "the circuit dial announced as RelayCircuit was accepted rather than refused \
         synchronously",
    );

    // AGAIN THE OUTCOME. R7.9 through R7.12 assert a completed relayed
    // path with Identify across it; a fixed budget makes them a load
    // measurement.
    {
        let mut nodes = [&mut dest, &mut relay_node, &mut source];
        pump_until(&mut nodes, Duration::from_secs(30), |n| {
            n[1].observed
                .details("relay-server")
                .iter()
                .any(|d| d.contains("CircuitReqAccepted"))
                && n[2].observed.connected.contains(&dest_peer)
                && n[2].observed.identify_protocols.contains_key(&dest_peer)
        })
        .await;
    }

    report.note(
        "R7.7",
        format!(
            "source gate: allowed {:?}, refused {:?}",
            source.ledger.allowed_by_origin(),
            source.ledger.refusals()
        ),
    );
    report.require(
        "R7.8",
        source
            .ledger
            .allowed_by_origin()
            .get("relay-circuit")
            .copied()
            .unwrap_or(0)
            > 0,
        "the gate admitted the circuit UNDER `relay-circuit`, so the origin the command \
         path sets is the one the policy judged",
    );

    // THE END IDENTITY, and it is the whole of ADR-0036's relayed
    // clause. The source's connection is to the DESTINATION's PeerId,
    // authenticated over the circuit, and the relay's PeerId is a
    // different peer it is separately connected to.
    // THE PATH WAS THE RELAY'S, which "connected to the destination"
    // does not say on its own — DCUtR may upgrade a relayed connection
    // to a direct one, and on loopback it does. Asserted at the relay,
    // which is the only party that can say a circuit carried this.
    report.require(
        "R7.12",
        relay_node
            .observed
            .details("relay-server")
            .iter()
            .any(|d| d.contains("CircuitReqAccepted"))
            && source
                .observed
                .details("relay-circuit-outbound")
                .iter()
                .any(|d| d.contains("OutboundCircuitEstablished")),
        "the relay accepted the circuit and the source established it, so the connection \
         below was reached THROUGH the relay",
    );
    report.require(
        "R7.9",
        source.observed.connected.contains(&dest_peer),
        "the source is connected to the DESTINATION's authenticated PeerId, not merely to \
         the relay it reached it through",
    );
    report.require(
        "R7.10",
        dest_peer != relay_peer && source.observed.connected.contains(&relay_peer),
        "and to the relay's, separately — two distinct authenticated identities, which is \
         what makes \"evaluated against the end PeerId\" a decidable rule",
    );
    // NOT MERELY CONNECTED: Identify completing over the relayed
    // connection is the end peer proving its own identity through the
    // relay, which a connection count alone cannot show.
    report.require(
        "R7.11",
        source.observed.identify_protocols.contains_key(&dest_peer),
        &format!(
            "Identify completed with the destination through the circuit: {:?}",
            source
                .observed
                .identify_protocols
                .get(&dest_peer)
                .map(|p| p.len())
        ),
    );
}

/// The address pair a real relayed inbound connection presented.
///
/// Handed to R9 so its fixture is the shape the wire delivered rather
/// than a format string that agrees with the author. Review finding on
/// PR #69: D3's force depends on the source PeerId being IN that
/// address, and a hand-written remote cannot be evidence of that.
#[derive(Debug, Clone)]
pub struct RelayedInbound {
    pub local: String,
    pub remote: String,
    pub source: String,
}

/// R8 — what a relayed inbound connection presents before anything is
/// authenticated.
///
/// `contracts/CONNECTIVITY.md` §10 requires relayed pre-Noise resource
/// use to be charged to the relay connection and the relay's PeerId
/// plus the global caps, explicitly NOT to a pseudo-source bucket. That
/// rule rests on a premise about the wire — that an arriving relayed
/// connection carries no original source IP — and on a capability —
/// that the receiver can tell a relayed connection from a direct one,
/// and name its relay, at the moment it must decide.
///
/// Both are measurable here, and the second is the one a design would
/// not tell you: `handle_pending_inbound_connection` runs before Noise,
/// so whatever it is handed is all the accounting can key on.
pub async fn r8_relayed_inbound_accounting(report: &mut Report) -> Option<RelayedInbound> {
    let mut relay_node = Node::new(Roles::infrastructure(), &[], &[]);
    let relay_addr = relay_node.listen().await;
    relay_node.swarm.add_external_address(relay_addr.clone());
    let relay_id = relay_node.identity.clone();
    let relay_peer = relay_node.peer_id;

    let mut dest = Node::new(Roles::client(), &[], &[]);
    let dest_direct = dest.listen().await;
    let dest_peer = dest.peer_id;
    let dest_id = dest.identity.clone();
    dest.add_relay(relay_peer);
    dest.swarm.add_peer_address(relay_peer, relay_addr.clone());
    let circuit = circuit_addr(&relay_addr, &relay_peer);
    let _ = dest.swarm.listen_on(circuit.clone());

    let mut source = Node::new(Roles::client(), &[], &[]);
    source.set_trust_sets(std::slice::from_ref(&dest_id), &[relay_id]);
    let _ = source.listen().await;
    let source_peer = source.peer_id;
    source.add_relay(relay_peer);
    source
        .swarm
        .add_peer_address(relay_peer, relay_addr.clone());
    // The destination must accept the source over the circuit; it is
    // the relay it holds as infrastructure.
    dest.set_trust_sets(&[source.identity.clone()], &[relay_node.identity.clone()]);

    {
        let mut nodes = [&mut dest, &mut relay_node, &mut source];
        pump_until(&mut nodes, Duration::from_secs(20), |n| {
            n[1].observed
                .details("relay-server")
                .iter()
                .any(|d| d.contains("ReservationReqAccepted"))
        })
        .await;
    }

    let via_relay: Multiaddr = format!("{circuit}/p2p/{dest_peer}")
        .parse()
        .expect("a circuit address naming the destination");
    let _ = source.dial_circuit(dest_peer, via_relay);

    {
        let mut nodes = [&mut dest, &mut relay_node, &mut source];
        // Wait for ESTABLISHMENT, not merely for the pending hook. The
        // first version stopped at the pending hook and then pumped a
        // pair that did not include the relay, so the connection never
        // completed and the established-inbound ledger was empty — an
        // absence that would have read as "the hook is not called for a
        // relayed connection", which is false.
        pump_until(&mut nodes, Duration::from_secs(20), |n| {
            !n[0].ledger.established_inbound().is_empty()
        })
        .await;
    }

    let inbound = dest.ledger.pending_inbound();
    report.note(
        "R8.1",
        format!("destination pending-inbound hooks: {inbound:?}"),
    );
    let established = dest.ledger.established_inbound();
    report.note(
        "R8.2",
        format!("destination established-inbound hooks: {established:?}"),
    );

    // ADR-0036's relayed sentence, measured on the side it is written
    // about: "inbound relayed DESTINATION connections are evaluated
    // against the authenticated remote application PeerId, not merely
    // the relay PeerId." R7 showed the outbound half. Here the
    // destination's established hook is handed the SOURCE's PeerId over
    // a local address that is the relay's — two different identities,
    // in one call, so a rule naming either is decidable.
    report.require(
        "R8.9",
        !established.is_empty()
            && established.iter().any(|(peer, local, _)| {
                peer == &source_peer.to_string() && local.contains("p2p-circuit")
            }),
        &format!(
            "the destination's established hook names the SOURCE's authenticated PeerId on a relayed local address, so the end identity is available where the trust decision belongs: {established:?}"
        ),
    );
    report.require(
        "R8.10",
        !established.is_empty()
            && established
                .iter()
                .all(|(peer, _, _)| peer != &relay_peer.to_string()),
        "and it is not the relay's PeerId — the party that carried the connection is not the party the connection is from",
    );

    let relayed: Vec<&(String, String)> = inbound
        .iter()
        .filter(|(local, _)| local.contains("p2p-circuit"))
        .collect();
    report.require(
        "R8.3",
        !relayed.is_empty(),
        "a relayed connection reached the destination's PENDING inbound hook, which is \
         where §10's pre-Noise accounting must decide",
    );

    // THE PREMISE §10 RESTS ON. No IP anywhere in what the receiver is
    // handed for the remote — so a per-source-IP bucket is not merely
    // discouraged, it is unbuildable.
    report.require(
        "R8.4",
        !relayed.is_empty()
            && relayed
                .iter()
                .all(|(_, remote)| !remote.contains("/ip4/") && !remote.contains("/ip6/")),
        &format!(
            "the relayed remote address carries no source IP: {:?}",
            relayed.iter().map(|(_, r)| r).collect::<Vec<_>>()
        ),
    );
    // AND IT CARRIES THE SOURCE'S PeerId, which is D3's entire force.
    // Review finding on PR #69: R8.4 asserted only that no IP was
    // present, so a future rendering with no source component at all
    // would keep it green while D3 — "identities are free to mint" —
    // became a claim about an address that never arrives.
    report.require(
        "R8.11",
        !relayed.is_empty()
            && relayed
                .iter()
                .all(|(_, remote)| remote.contains(&source_peer.to_string())),
        &format!(
            "the relayed remote address IS the source's PeerId, so the bucket \
             `source_label` derives from it is one per identity: {:?}",
            relayed.iter().map(|(_, r)| r).collect::<Vec<_>>()
        ),
    );

    // THE CAPABILITY §10 NEEDS. The relay is nameable from the LOCAL
    // address before authentication, so "charge the relay connection
    // and the relay's PeerId" is decidable at the only moment it can
    // be.
    report.require(
        "R8.5",
        !relayed.is_empty()
            && relayed
                .iter()
                .all(|(local, _)| local.contains(&relay_peer.to_string())),
        &format!(
            "the relay's PeerId is present in the local address at the pending hook, so the \
             bucket §10 names can be chosen there: {:?}",
            relayed.iter().map(|(l, _)| l).collect::<Vec<_>>()
        ),
    );

    // THE CONTROL, and without it "no IP" says nothing: a DIRECT
    // inbound connection to the same node, through the same hook, DOES
    // carry one. So the absence is the relayed path's, not this
    // fixture's.
    let mut direct = Node::new(Roles::client(), &[dest_id], &[]);
    let _ = direct.listen().await;
    dest.set_trust_sets(
        &[source.identity.clone(), direct.identity.clone()],
        &[relay_node.identity.clone()],
    );
    let _ = direct.dial(dest_peer, dest_direct);
    {
        let mut nodes = [&mut dest, &mut direct];
        pump(&mut nodes, Duration::from_secs(6)).await;
    }
    let after = dest.ledger.pending_inbound();
    let direct_inbound: Vec<&(String, String)> = after
        .iter()
        .filter(|(local, _)| !local.contains("p2p-circuit"))
        .collect();
    report.note(
        "R8.6",
        format!("destination direct pending-inbound hooks: {direct_inbound:?}"),
    );
    report.require(
        "R8.7",
        !direct_inbound.is_empty()
            && direct_inbound
                .iter()
                .all(|(_, remote)| remote.contains("/ip4/")),
        "the control: a DIRECT inbound connection through the same hook DOES carry a source \
         IP, so R8.4's absence is the relayed path and not this fixture",
    );

    // AND THE TWO ARE DISTINGUISHABLE, which is what lets one hook
    // apply two accounting rules.
    report.require(
        "R8.8",
        !relayed.is_empty()
            && !direct_inbound.is_empty()
            && relayed
                .iter()
                .all(|(local, _)| local.contains("p2p-circuit"))
            && direct_inbound
                .iter()
                .all(|(local, _)| !local.contains("p2p-circuit")),
        "relayed and direct inbound connections are told apart at the pending hook by the \
         local address alone",
    );

    relayed.first().map(|(local, remote)| RelayedInbound {
        local: local.clone(),
        remote: remote.clone(),
        source: source_peer.to_string(),
    })
}

/// R9 — the pre-auth bucket a relayed inbound connection is charged to.
///
/// `contracts/CONNECTIVITY.md` §10 is explicit: where the original
/// client IP is unavailable, the destination MUST charge the
/// **authenticated relay transport connection / relay PeerId** plus the
/// global caps, and **MUST NOT create unbounded pseudo-source buckets
/// from circuit metadata.**
///
/// R8 measured what the hook is handed: a remote address of
/// `/p2p/<source>` with no IP anywhere. This asks what the SHIPPED
/// `PreAuthAdmission` — production, by path, unmodified — does with
/// that, by calling its `handle_pending_inbound_connection` with the
/// exact address shapes R8 observed on the wire.
///
/// The per-source ceiling is set to one, so "a second connection from
/// the same bucket is refused" is a single call rather than a loop.
pub async fn r9_relayed_preauth_bucket(report: &mut Report, observed: Option<RelayedInbound>) {
    use interweave_transport_libp2p::PreAuthAdmission;
    use interweave_transport_runtime::preauth::PreAuthLimitsBuilder;
    use libp2p::swarm::{ConnectionId, NetworkBehaviour};

    // THE SHAPES R8 ACTUALLY OBSERVED, carried here rather than
    // retyped. Review finding on PR #69: a hand-written
    // `format!("/p2p/{src}")` agrees with the author, so a rendering
    // change on the wire would leave this experiment green while D3
    // described an address that no longer arrives.
    let Some(observed) = observed else {
        report.require(
            "R9.0",
            false,
            "R8 observed no relayed inbound, so this experiment has no measured address \
             shape to work from and refuses to invent one",
        );
        return;
    };
    report.note(
        "R9.5",
        format!(
            "fixture taken from R8's measurement: local={:?}, remote={:?}",
            observed.local, observed.remote
        ),
    );

    let limits = PreAuthLimitsBuilder {
        max_pending_per_source: 1,
        max_pending_total: 8,
        ..PreAuthLimitsBuilder::default()
    }
    .build()
    .expect("valid limits");
    let mut gate = PreAuthAdmission::new(limits);

    let fresh_peer = || {
        libp2p::PeerId::from_public_key(&libp2p::identity::Keypair::generate_ed25519().public())
            .to_string()
    };
    // A SECOND SOURCE, minted the way an attacker mints one: the same
    // address with a different identity in it. Substituting into the
    // observed string is what keeps the shape the wire's rather than
    // this file's.
    let relayed_local: Multiaddr = observed.local.parse().expect("R8's local address");
    let remote_one: Multiaddr = observed.remote.parse().expect("R8's remote address");
    let remote_two: Multiaddr = observed
        .remote
        .replace(&observed.source, &fresh_peer())
        .parse()
        .expect("the same shape with another identity");
    report.require(
        "R9.6",
        remote_two != remote_one,
        &format!(
            "the second source differs from the first only in identity ({remote_one} vs \
             {remote_two}), so what follows varies exactly one thing"
        ),
    );

    let first = gate.handle_pending_inbound_connection(
        ConnectionId::new_unchecked(1),
        &relayed_local,
        &remote_one,
    );
    let second = gate.handle_pending_inbound_connection(
        ConnectionId::new_unchecked(2),
        &relayed_local,
        &remote_two,
    );
    report.note(
        "R9.1",
        format!(
            "two relayed inbounds over ONE relay, different source PeerIds: first={:?}, \
             second={:?}",
            first.is_ok(),
            second.is_ok()
        ),
    );

    // THE CONTROL FIRST, because without it "the second was admitted"
    // could mean the ceiling is not enforced at all. Two DIRECT
    // inbounds from one IP: the second is refused, so
    // `max_pending_per_source: 1` is doing its job.
    let mut direct_gate = PreAuthAdmission::new(limits);
    let direct_local: Multiaddr = "/ip4/127.0.0.1/tcp/4001".parse().expect("local");
    let direct_remote: Multiaddr = "/ip4/198.51.100.7/tcp/5001".parse().expect("remote");
    let direct_first = direct_gate.handle_pending_inbound_connection(
        ConnectionId::new_unchecked(3),
        &direct_local,
        &direct_remote,
    );
    let direct_second = direct_gate.handle_pending_inbound_connection(
        ConnectionId::new_unchecked(4),
        &direct_local,
        &"/ip4/198.51.100.7/tcp/5002".parse().expect("remote"),
    );
    report.require(
        "R9.2",
        direct_first.is_ok() && direct_second.is_err(),
        &format!(
            "the control: two direct inbounds from ONE IP — the second is refused, so the \
             per-source ceiling is enforced (first={:?}, second={:?})",
            direct_first.is_ok(),
            direct_second.is_ok()
        ),
    );

    // THE DIVERGENCE. The same ceiling, the same gate, one relay
    // connection — and each source PeerId gets a bucket of its own.
    // PeerIds are free to mint, so the bucket count is chosen by
    // whoever is attacking, which is what "unbounded pseudo-source
    // buckets from circuit metadata" names.
    report.require(
        "R9.3",
        first.is_ok() && second.is_ok(),
        &format!(
            "TODAY'S BEHAVIOUR, pinned so a fix fails here rather than passing silently: two \
             relayed inbounds over one relay each get their own bucket (first={:?}, \
             second={:?})",
            first.is_ok(),
            second.is_ok()
        ),
    );
    if first.is_ok() && second.is_ok() {
        report.divergence(
            "D3",
            "PreAuthAdmission buckets a relayed inbound by the SOURCE PeerId carried in \
             the circuit's remote address. `source_label` returns the multiaddr as \
             written when it holds no IP, and R8 measured that address to be \
             `/p2p/<source>` — so one relay connection yields one bucket per source \
             identity, and identities are free to mint. The relay's own PeerId is present \
             in the LOCAL address (R8.5) and is not read",
            "contracts/CONNECTIVITY.md §10, which requires charging the authenticated \
             relay transport connection / relay PeerId plus the global caps and says the \
             destination MUST NOT create unbounded pseudo-source buckets from circuit \
             metadata. Not reachable in a shipped build today — no relay feature is \
             compiled — and live the moment Stage 11's Phase 4 lands, which is why it is \
             recorded against the code rather than against the stage",
        );
    }

    // AND THE GLOBAL CAP IS THE ONLY THING LEFT STANDING. Worth
    // measuring rather than assuming: if it too were per-bucket, there
    // would be no bound at all.
    let mut exhaust = PreAuthAdmission::new(limits);
    let mut admitted = 0_usize;
    for i in 0..32_usize {
        let remote: Multiaddr = observed
            .remote
            .replace(&observed.source, &fresh_peer())
            .parse()
            .expect("the same shape with another identity");
        if exhaust
            .handle_pending_inbound_connection(
                ConnectionId::new_unchecked(100 + i),
                &relayed_local,
                &remote,
            )
            .is_ok()
        {
            admitted += 1;
        }
    }
    report.require(
        "R9.4",
        admitted == 8,
        &format!(
            "the GLOBAL ceiling still bounds the total ({admitted} of 32 admitted against \
             max_pending_total 8), so the failure is the bucket's granularity and not the \
             absence of any bound"
        ),
    );
}

/// R10 — reservations are held on several relays at once, and the
/// address one contributes is withdrawn when that relay is lost.
///
/// `transport/libp2p/CONNECTIVITY.md` §8 keys reservation targets to
/// reachability state (2 while Unknown or NotVerified, 1 when
/// VerifiedPublic, 4 at most) and requires reservation-derived
/// addresses to be advertised while live and withdrawn immediately on
/// loss. The targets are ours to schedule; what the crate has to
/// support for them to be expressible is holding more than one
/// reservation at a time, and giving up an address when its relay goes.
///
/// Both are measurable on loopback. Renewal is not: the crate's default
/// reservation is an hour long, so nothing in a run this short can
/// observe a refresh, and that is stated as a limit rather than
/// asserted from silence.
pub async fn r10_reservation_lifecycle(report: &mut Report) {
    let mut relay_a = Node::new(Roles::infrastructure(), &[], &[]);
    let addr_a = relay_a.listen().await;
    relay_a.swarm.add_external_address(addr_a.clone());
    let peer_a = relay_a.peer_id;

    let mut relay_b = Node::new(Roles::infrastructure(), &[], &[]);
    let addr_b = relay_b.listen().await;
    relay_b.swarm.add_external_address(addr_b.clone());
    let peer_b = relay_b.peer_id;

    let mut client = Node::new(
        Roles::client(),
        &[],
        &[relay_a.identity.clone(), relay_b.identity.clone()],
    );
    let _ = client.listen().await;
    client.add_relay(peer_a);
    client.add_relay(peer_b);
    client.swarm.add_peer_address(peer_a, addr_a.clone());
    client.swarm.add_peer_address(peer_b, addr_b.clone());

    let circuit_a = circuit_addr(&addr_a, &peer_a);
    let circuit_b = circuit_addr(&addr_b, &peer_b);
    let _ = client.swarm.listen_on(circuit_a);
    let _ = client.swarm.listen_on(circuit_b);

    {
        let mut nodes = [&mut client, &mut relay_a, &mut relay_b];
        pump_until(&mut nodes, Duration::from_secs(25), |n| {
            n[0].observed.count("relay-reservation-accepted") >= 2
        })
        .await;
    }

    report.note(
        "R10.1",
        format!(
            "client reservation events: {:?}",
            client.observed.details("relay-reservation-accepted")
        ),
    );
    report.require(
        "R10.2",
        client.observed.count("relay-reservation-accepted") >= 2,
        &format!(
            "the client holds reservations on TWO relays at once, so §8's target of two is \
             expressible on this crate ({} accepted)",
            client.observed.count("relay-reservation-accepted")
        ),
    );
    // AND BOTH RELAYS AGREE, which the client's own event count cannot
    // show: two accepted events could both come from one relay
    // renewing.
    report.require(
        "R10.3",
        relay_a
            .observed
            .details("relay-server")
            .iter()
            .any(|d| d.contains("ReservationReqAccepted"))
            && relay_b
                .observed
                .details("relay-server")
                .iter()
                .any(|d| d.contains("ReservationReqAccepted")),
        "each relay separately recorded accepting a reservation, so the two are distinct \
         relays rather than one renewing",
    );

    // THE ADDRESSES. A reservation's whole product is an address to
    // advertise, so the listen set is where the reservation is visible.
    let listening: Vec<String> = client
        .swarm
        .listeners()
        .map(std::string::ToString::to_string)
        .collect();
    report.note("R10.4", format!("client listen addresses: {listening:?}"));
    let via_a = listening
        .iter()
        .any(|a| a.contains("p2p-circuit") && a.contains(&peer_a.to_string()));
    let via_b = listening
        .iter()
        .any(|a| a.contains("p2p-circuit") && a.contains(&peer_b.to_string()));
    report.require(
        "R10.5",
        via_a && via_b,
        &format!("both reservation-derived addresses are advertised while live: {listening:?}"),
    );

    // THE LOSS. Relay B goes away entirely — process gone, socket gone
    // — which is the shape §8's "withdrawn immediately on loss" is
    // about.
    drop(relay_b);

    // WITHDRAWAL IS PART OF THE LOSS, not something that happens
    // eventually. Review finding on PR #69: an unconditional ten-second
    // pump followed by one sample shows only that the address is gone
    // by the end, and a stale-address window is exactly what makes
    // peers keep dialling a dead relay. So the loss is waited for
    // FIRST, and the address is then sampled against a stated bound.
    let closed = {
        let mut nodes = [&mut client, &mut relay_a];
        pump_until(&mut nodes, Duration::from_secs(10), |n| {
            n[0].observed.events.iter().any(|(label, detail)| {
                *label == "connection-closed" && detail == &peer_b.to_string()
            })
        })
        .await
    };
    report.require(
        "R10.9",
        closed,
        "the client observed the relay's connection closing, so what follows is measured \
         from the loss rather than from a timer",
    );

    // HOW LONG the address may still be advertised after the loss.
    //
    // Not zero: the withdrawal travels from the relay transport's
    // listener to the Swarm's listener set, and a sample taken inside
    // the same poll could race that hand-off. One second is two orders
    // of magnitude below the reservation lifetime and far below any
    // dial timeout, so an address surviving it is a stale window rather
    // than a scheduling artefact.
    const WITHDRAWAL_BOUND: Duration = Duration::from_secs(1);

    let withdrawn_within_bound = {
        let mut nodes = [&mut client, &mut relay_a];
        pump_until(&mut nodes, WITHDRAWAL_BOUND, |n| {
            !n[0].swarm.listeners().any(|a| {
                a.to_string().contains("p2p-circuit") && a.to_string().contains(&peer_b.to_string())
            })
        })
        .await
    };
    report.require(
        "R10.10",
        withdrawn_within_bound,
        "the lost relay's address is withdrawn WITHIN a second of the loss being observed, \
         so there is no stale window in which peers would keep dialling a dead relay",
    );

    let after: Vec<String> = client
        .swarm
        .listeners()
        .map(std::string::ToString::to_string)
        .collect();
    report.note(
        "R10.6",
        format!("client listen addresses after loss: {after:?}"),
    );
    report.require(
        "R10.7",
        !after
            .iter()
            .any(|a| a.contains("p2p-circuit") && a.contains(&peer_b.to_string())),
        &format!("the lost relay's reservation address is withdrawn: {after:?}"),
    );
    // THE CONTROL, and without it R10.7 would pass for a client that
    // dropped every relayed address, or all its listeners, when any
    // relay died.
    report.require(
        "R10.8",
        after
            .iter()
            .any(|a| a.contains("p2p-circuit") && a.contains(&peer_a.to_string())),
        &format!(
            "the surviving relay's address is still advertised, so R10.7 is one \
             reservation ending rather than the client giving up relaying: {after:?}"
        ),
    );
}

/// R11 — the relay server's budgets, as `RELAY.md` §8 sets them and as
/// the crate defaults them.
///
/// §8 is a table of eight ceilings. A stage that constructs
/// `relay::Behaviour::new(peer, Config::default())` gets none of them,
/// and the differences are not all in the safe direction — so this
/// records each one and asserts the two that would silently break a
/// deployment.
///
/// The per-peer ceilings are then MEASURED rather than read, because
/// the crate compares with `>` rather than `>=` and an off-by-one in a
/// resource limit is exactly what a table copied from a specification
/// would not reveal.
pub async fn r11_relay_server_budgets(report: &mut Report) {
    let defaults = libp2p::relay::Config::default();
    report.note(
        "R11.1",
        format!(
            "libp2p-relay 0.21.1 defaults: max_reservations={}, max_reservations_per_peer={}, \
             reservation_duration={:?}, max_circuits={}, max_circuits_per_peer={}, \
             max_circuit_duration={:?}, max_circuit_bytes={}",
            defaults.max_reservations,
            defaults.max_reservations_per_peer,
            defaults.reservation_duration,
            defaults.max_circuits,
            defaults.max_circuits_per_peer,
            defaults.max_circuit_duration,
            defaults.max_circuit_bytes,
        ),
    );

    // THE TWO THAT WOULD BREAK A DEPLOYMENT, not merely differ from it.
    // §8 asks for 64 MiB per circuit and an hour; the defaults are
    // 128 KiB and two minutes — three 48 KiB application payloads and
    // the circuit is spent.
    report.require(
        "R11.2",
        defaults.max_circuit_bytes < 64 * 1024 * 1024,
        &format!(
            "the default per-circuit byte budget ({} bytes) is far below RELAY.md §8's \
             64 MiB, so Phase 4 must set it rather than take the default",
            defaults.max_circuit_bytes
        ),
    );
    report.require(
        "R11.3",
        defaults.max_circuit_duration < Duration::from_secs(3600),
        &format!(
            "and the default circuit duration ({:?}) is below §8's 1h",
            defaults.max_circuit_duration
        ),
    );
    // AND THE TWO THAT ARE LOOSER THAN THE SPECIFICATION, which is the
    // direction that matters for a budget.
    report.require(
        "R11.4",
        defaults.max_reservations > 64 && defaults.max_reservations_per_peer > 1,
        &format!(
            "the default reservation ceilings ({} total, {} per peer) are LOOSER than §8's \
             64 and 1",
            defaults.max_reservations, defaults.max_reservations_per_peer
        ),
    );
    // AND ONE §8 NAMES THAT THE CRATE DOES NOT HAVE. `Config` carries
    // no pending-control-operations ceiling at all, so §8's
    // `max_pending_control` cannot be expressed by configuring this
    // behaviour — it needs a wrapper or an amendment, and Phase 4 must
    // decide which.
    report.note(
        "R11.5",
        "RELAY.md §8's max_pending_control has no field in relay::Config; the struct's \
         seven public knobs are the reservation and circuit ones above plus the two rate \
         limiter vectors"
            .to_owned(),
    );

    // THE OFF-BY-ONE, measured. The crate refuses when
    // `num_circuits_of_peer(src) > max_circuits_per_peer`, so a ceiling
    // of one admits two. Set it to one, open two circuits from one
    // source to two destinations, and count what the relay accepted.
    let mut relay_node = Node::with_relay_config(
        Roles::infrastructure(),
        &[],
        &[],
        libp2p::relay::Config {
            max_circuits_per_peer: 1,
            ..libp2p::relay::Config::default()
        },
    );
    let relay_addr = relay_node.listen().await;
    relay_node.swarm.add_external_address(relay_addr.clone());
    let relay_id = relay_node.identity.clone();
    let relay_peer = relay_node.peer_id;
    let circuit = circuit_addr(&relay_addr, &relay_peer);

    let mut dest_a = Node::new(Roles::client(), std::slice::from_ref(&relay_id), &[]);
    let _ = dest_a.listen().await;
    dest_a.add_relay(relay_peer);
    dest_a
        .swarm
        .add_peer_address(relay_peer, relay_addr.clone());
    let _ = dest_a.swarm.listen_on(circuit.clone());
    let dest_a_peer = dest_a.peer_id;

    let mut dest_b = Node::new(Roles::client(), std::slice::from_ref(&relay_id), &[]);
    let _ = dest_b.listen().await;
    dest_b.add_relay(relay_peer);
    dest_b
        .swarm
        .add_peer_address(relay_peer, relay_addr.clone());
    let _ = dest_b.swarm.listen_on(circuit.clone());
    let dest_b_peer = dest_b.peer_id;

    // A THIRD, and it is what turns "two were accepted" into "the
    // ceiling bites, one later than it reads". Without it, two
    // admissions under a ceiling of one are equally consistent with the
    // ceiling being ignored altogether.
    let mut dest_c = Node::new(Roles::client(), std::slice::from_ref(&relay_id), &[]);
    let _ = dest_c.listen().await;
    dest_c.add_relay(relay_peer);
    dest_c
        .swarm
        .add_peer_address(relay_peer, relay_addr.clone());
    let _ = dest_c.swarm.listen_on(circuit.clone());
    let dest_c_peer = dest_c.peer_id;

    let mut source = Node::new(
        Roles::client(),
        &[
            dest_a.identity.clone(),
            dest_b.identity.clone(),
            dest_c.identity.clone(),
        ],
        &[relay_id],
    );
    let _ = source.listen().await;
    source.add_relay(relay_peer);
    source
        .swarm
        .add_peer_address(relay_peer, relay_addr.clone());

    {
        let mut nodes = [
            &mut dest_a,
            &mut dest_b,
            &mut dest_c,
            &mut relay_node,
            &mut source,
        ];
        pump_until(&mut nodes, Duration::from_secs(30), |n| {
            n[3].observed
                .details("relay-server")
                .iter()
                .filter(|d| d.contains("ReservationReqAccepted"))
                .count()
                >= 3
        })
        .await;
    }

    /// Circuit requests this relay has answered — accepted or denied.
    fn answered(relay: &Node) -> usize {
        relay
            .observed
            .details("relay-server")
            .iter()
            .filter(|d| d.contains("CircuitReqAccepted") || d.contains("CircuitReqDenied"))
            .count()
    }

    // ONE AT A TIME, with the first settled before the second is
    // asked for. Two circuit dials issued together do not test a
    // ceiling: the relay counts circuits it has ACCEPTED, so a second
    // request that arrives while the first is still negotiating is
    // counted against nothing.
    let mut dial_results = Vec::new();
    for dest in [dest_a_peer, dest_b_peer, dest_c_peer] {
        let via: Multiaddr = format!("{circuit}/p2p/{dest}")
            .parse()
            .expect("a circuit address naming a destination");
        dial_results.push(format!("{:?}", source.dial_circuit(dest, via).is_ok()));
        // WAIT FOR THE RELAY TO ANSWER THIS ONE. The ceiling counts
        // circuits the relay has ACCEPTED, so a request still in flight
        // is counted against nothing — which is how the first version
        // of this experiment concluded the ceiling held. A fixed budget
        // makes that a race rather than a sequence, so the wait is for
        // the answer: one more accept-or-deny than before the dial.
        let answered_before = answered(&relay_node);
        let mut nodes = [
            &mut dest_a,
            &mut dest_b,
            &mut dest_c,
            &mut relay_node,
            &mut source,
        ];
        pump_until(&mut nodes, Duration::from_secs(20), |n| {
            answered(n[3]) > answered_before
        })
        .await;
    }
    report.note(
        "R11.8",
        format!("circuit dials accepted by the swarm: {dial_results:?}"),
    );

    let accepted = relay_node
        .observed
        .details("relay-server")
        .iter()
        .filter(|d| d.contains("CircuitReqAccepted"))
        .count();
    report.note(
        "R11.6",
        format!(
            "with max_circuits_per_peer = 1, circuits accepted from ONE source: {accepted}; \
             relay events: {:?}",
            relay_node.observed.details("relay-server")
        ),
    );
    let denied = relay_node
        .observed
        .details("relay-server")
        .iter()
        .filter(|d| d.contains("CircuitReqDenied"))
        .count();
    report.require(
        "R11.7",
        accepted == 2,
        &format!(
            "MEASURED OFF-BY-ONE: a per-source ceiling of one admits TWO circuits \
             ({accepted} accepted from three asked for), because the crate refuses on `>` \
             rather than `>=`. Every per-peer number copied from RELAY.md §8 is therefore \
             one higher than it reads"
        ),
    );
    report.require(
        "R11.9",
        denied > 0,
        &format!(
            "and the third IS refused ({denied} denial(s)), so the ceiling is enforced one \
             later than it reads rather than not enforced at all — which is the difference \
             between an off-by-one and a missing check"
        ),
    );
}

/// R12 — DCUtR has no bounds of its own, and a successful punch is a
/// second connection to a peer already connected.
///
/// `DCUTR.md` and `transport/libp2p/CONNECTIVITY.md` §13 set three bounds: at most four
/// concurrent hole punches, one per peer, and a five-minute cooldown
/// after failure. None of them is a knob. `dcutr::Behaviour::new` takes
/// a `PeerId` and nothing else, and the crate's own ceiling —
/// `MAX_NUMBER_OF_UPGRADE_ATTEMPTS = 3` — is a `pub(crate)` constant
/// counting retries per relayed connection, which is neither a
/// concurrency cap nor a cooldown.
///
/// So the bounds must be enforced outside the behaviour. The dial gate
/// is the only place that sees every dial — but whether a dial count
/// can BE an attempt count is the question this asks, and the answer
/// shapes Phase 6: both ends dial for one punch and only one of them is
/// told how it ended.
///
/// `contracts/CONNECTIVITY.md` §5 is the other half: *"a successful DCUtR hole punch for an
/// already-connected relayed peer therefore does not emit a second
/// `PeerConnected`."* That is a rule about our event, and it exists
/// because of what the Swarm below does.
pub async fn r12_dcutr_bounds(report: &mut Report) {
    let mut relay_node = Node::new(Roles::infrastructure(), &[], &[]);
    let relay_addr = relay_node.listen().await;
    relay_node.swarm.add_external_address(relay_addr.clone());
    let relay_id = relay_node.identity.clone();
    let relay_peer = relay_node.peer_id;

    let mut dest = Node::new(Roles::client(), std::slice::from_ref(&relay_id), &[]);
    let _ = dest.listen().await;
    dest.add_relay(relay_peer);
    dest.swarm.add_peer_address(relay_peer, relay_addr.clone());
    let circuit = circuit_addr(&relay_addr, &relay_peer);
    let _ = dest.swarm.listen_on(circuit.clone());
    let dest_peer = dest.peer_id;
    let dest_id = dest.identity.clone();

    let mut source = Node::new(Roles::client(), &[], &[]);
    source.set_trust_sets(&[dest_id], &[relay_id]);
    let _ = source.listen().await;
    source.add_relay(relay_peer);
    source
        .swarm
        .add_peer_address(relay_peer, relay_addr.clone());
    dest.set_trust_sets(&[source.identity.clone()], &[relay_node.identity.clone()]);

    {
        let mut nodes = [&mut dest, &mut relay_node, &mut source];
        pump_until(&mut nodes, Duration::from_secs(25), |n| {
            n[1].observed
                .details("relay-server")
                .iter()
                .any(|d| d.contains("ReservationReqAccepted"))
        })
        .await;
    }

    let via_relay: Multiaddr = format!("{circuit}/p2p/{dest_peer}")
        .parse()
        .expect("a circuit address naming the destination");
    let _ = source.dial_circuit(dest_peer, via_relay);
    {
        let mut nodes = [&mut dest, &mut relay_node, &mut source];
        // WAIT FOR WHAT R12.7 AND R12.8 ASSERT: a dcutr result at
        // either end AND both connections open at the source. Waiting
        // for the event and then pumping a fixed eight seconds makes
        // those two a statement about scheduling.
        pump_until(&mut nodes, Duration::from_secs(35), |n| {
            let punched = !n[0].observed.details("dcutr").is_empty()
                || !n[2].observed.details("dcutr").is_empty();
            let relayed = n[2]
                .observed
                .open_connections
                .values()
                .any(|(peer, relayed)| *peer == dest_peer && *relayed);
            let direct = n[2]
                .observed
                .open_connections
                .values()
                .any(|(peer, relayed)| *peer == dest_peer && !*relayed);
            punched && relayed && direct
        })
        .await;
    }

    let punches: Vec<String> = source
        .observed
        .details("dcutr")
        .into_iter()
        .chain(dest.observed.details("dcutr"))
        .collect();
    report.note(
        "R12.1",
        format!("dcutr events across both ends: {punches:?}"),
    );
    report.require(
        "R12.2",
        !punches.is_empty(),
        "a hole punch was attempted over the relayed path, so what follows is about a real \
         upgrade rather than about nothing happening",
    );

    // THE COUNTING PROBLEM, as this run measures it. Every hole-punch
    // dial is attributed and reaches the gate — F1's mechanism doing
    // its job — but a gate counting dials still cannot enforce §13,
    // because it is never told how an attempt ended: both ends dial and
    // only one of them is told the result (R12.5). Candidate
    // multiplicity is a second reason to expect the same and is NOT
    // measured here; on loopback each endpoint dials once.
    let punch_dials = source
        .ledger
        .allowed_by_origin()
        .get("dcutr-hole-punch")
        .copied()
        .unwrap_or(0)
        + dest
            .ledger
            .allowed_by_origin()
            .get("dcutr-hole-punch")
            .copied()
            .unwrap_or(0);
    let punch_targets = source
        .attribution
        .targets(interweave_transport_runtime::DialOrigin::DcutrHolePunch)
        .len()
        + dest
            .attribution
            .targets(interweave_transport_runtime::DialOrigin::DcutrHolePunch)
            .len();
    let source_dials = source
        .ledger
        .allowed_by_origin()
        .get("dcutr-hole-punch")
        .copied()
        .unwrap_or(0);
    let dest_dials = dest
        .ledger
        .allowed_by_origin()
        .get("dcutr-hole-punch")
        .copied()
        .unwrap_or(0);
    report.note(
        "R12.3",
        format!(
            "hole-punch dials admitted: source {source_dials}, destination {dest_dials} \
             (total {punch_dials}, {punch_targets} target entries); dcutr events: source \
             {}, destination {}",
            source.observed.details("dcutr").len(),
            dest.observed.details("dcutr").len()
        ),
    );
    // EVERY dial, not at least one. Review finding on PR #69:
    // `punch_dials > 0` is satisfied by a single admitted dial while
    // any others — a second endpoint's, a retry, a further candidate —
    // go unattributed, which is the exact regression F12's conclusion
    // depends on not happening. The claim is therefore the three
    // numbers agreeing: announced == resolved for this origin on each
    // node, and zero dials met with no note at all.
    let announced_punches = source
        .attribution
        .announced()
        .get("dcutr-hole-punch")
        .copied()
        .unwrap_or(0)
        + dest
            .attribution
            .announced()
            .get("dcutr-hole-punch")
            .copied()
            .unwrap_or(0);
    let resolved_punches = source
        .attribution
        .resolved()
        .get("dcutr-hole-punch")
        .copied()
        .unwrap_or(0)
        + dest
            .attribution
            .resolved()
            .get("dcutr-hole-punch")
            .copied()
            .unwrap_or(0);
    let unattributed = source.attribution.unattributed() + dest.attribution.unattributed();
    report.note(
        "R12.9",
        format!(
            "hole-punch dials announced {announced_punches}, resolved at a gate {resolved_punches}, unattributed dials of any origin {unattributed}"
        ),
    );
    report.require(
        "R12.4",
        punch_dials > 0
            && announced_punches > 0
            && resolved_punches == announced_punches
            && unattributed == 0,
        &format!(
            "EVERY hole-punch dial reached a gate under `dcutr-hole-punch` — announced {announced_punches}, resolved {resolved_punches}, {unattributed} dial(s) of any origin met the gate with no note — so §13's bounds have somewhere to be enforced at all"
        ),
    );
    // ONE PUNCH IS TWO DIALS AT TWO NODES, and that is the whole of
    // what this asserts.
    //
    // An earlier version required that EXACTLY ONE end reported the
    // result, having seen that in several runs. It is not true: run it
    // enough times and both ends report. The claim was a shape three
    // runs happened to have, which is the same mistake as measuring a
    // fixture — so it is gone, and the per-node result counts are a
    // note (R12.13) rather than a requirement.
    //
    // What holds every time is the split. A logical hole punch is ONE
    // attempt and produces a dial at each end, so no single node's gate
    // ever sees the attempt — it sees its own half. A per-peer "one
    // hole punch" rule counted at one gate is counting half of
    // something, and the outcome that would start a cooldown is
    // delivered to the DCUtR behaviour rather than to the gate, whether
    // or not both ends happen to get it.
    //
    // Candidate multiplicity is NOT measured either: each endpoint
    // dialled once, because loopback offers one address.
    let source_events = source.observed.details("dcutr").len();
    let dest_events = dest.observed.details("dcutr").len();
    report.require(
        "R12.5",
        source_dials > 0 && dest_dials > 0,
        &format!(
            "ONE hole punch produces a dial at BOTH ends (source {source_dials}, \
             destination {dest_dials}), so no single node's gate sees the attempt — only \
             its own half. A per-peer attempt ceiling counted at one gate counts half of \
             something, which is why §13's bounds cannot be a gate counting dials"
        ),
    );
    report.note(
        "R12.13",
        format!(
            "results reported per node this run: source {source_events}, destination \
             {dest_events}. NOT asserted — an earlier version required exactly one end to \
             report, and a later run had both. Whether a node learns its own attempt's \
             outcome is not a property this harness can pin, which is itself a reason a \
             cooldown cannot be keyed on it at the gate"
        ),
    );

    // §5, AND WHY IT IS A RULE AT ALL. The Swarm reports a second
    // `ConnectionEstablished` for a peer that was already connected:
    // the relayed connection and the direct one are two connections,
    // and libp2p says so both times. Nothing deduplicates them, so
    // "does not emit a second PeerConnected" is work Phase 6 owes
    // rather than a property inherited from the library.
    let established_to_dest = source
        .observed
        .events
        .iter()
        .filter(|(label, detail)| {
            *label == "connection-established" && detail == &dest_peer.to_string()
        })
        .count();
    report.note(
        "R12.6",
        format!(
            "source ConnectionEstablished events naming the destination: {established_to_dest}"
        ),
    );
    report.require(
        "R12.7",
        established_to_dest >= 2,
        &format!(
            "the Swarm reports a SECOND connection to an already-connected peer when the \
             punch succeeds ({established_to_dest} for one logical peer), so §5's \"no \
             second PeerConnected\" is a rule the runtime must implement rather than one \
             the library provides"
        ),
    );
    // AND THE RELAYED PATH IS NOT TORN DOWN BY THE UPGRADE, which §13's
    // fallback rule depends on: the peer stays connected throughout,
    // and there is no window where it is not.
    // THE RELAYED CONNECTION ITSELF, by id, not the peer's presence in
    // a set. Review finding on PR #69: `connected` holds PeerIds, so a
    // relayed connection that closed as the direct one opened leaves
    // the peer present and the assertion passing — which is the
    // opposite of the fallback §13 requires. The claim is that BOTH
    // connections to this peer are open at once.
    let open_relayed = source
        .observed
        .open_connections
        .values()
        .filter(|(peer, relayed)| *peer == dest_peer && *relayed)
        .count();
    let open_direct = source
        .observed
        .open_connections
        .values()
        .filter(|(peer, relayed)| *peer == dest_peer && !*relayed)
        .count();
    report.note(
        "R12.10",
        format!(
            "open connections to the destination: {open_relayed} relayed, {open_direct} direct"
        ),
    );
    report.require(
        "R12.8",
        open_relayed > 0 && open_direct > 0,
        &format!(
            "the RELAYED connection is still open beside the new direct one ({open_relayed} relayed, {open_direct} direct) — the upgrade added a path rather than replacing one, which is what §13's fallback rule needs"
        ),
    );
}
