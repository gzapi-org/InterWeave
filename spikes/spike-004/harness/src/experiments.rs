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
    for expected in [
        "/libp2p/autonat/2/dial-request",
        "/libp2p/circuit/relay/0.2.0/hop",
    ] {
        report.require(
            "R1.7",
            advertised.iter().any(|p| p == expected),
            &format!("an infrastructure node advertises {expected}"),
        );
    }
}

/// R2 — every behaviour-originated dial is attributable.
///
/// THE QUESTION STAGE 11 CANNOT PROCEED WITHOUT. Production's pending
/// hook is handed a `ConnectionId`, an `Option<PeerId>` and an empty
/// address list, and today infers `KademliaQuery` because Kademlia is
/// the only behaviour that can dial. With three more, that inference
/// is wrong for every one of them — and wrong in the direction that
/// fails closed against the infrastructure the stack needs, because
/// `KademliaQuery.is_data_plane()` is true.
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
    // AutoNAT probe control, and relay reservation/circuit control.
    for origin in [
        DialOrigin::RelayReservation,
        DialOrigin::RelayCircuit,
        DialOrigin::AutonatProbe,
    ] {
        let outcome = ask(origin);
        report.require(
            "R3.1",
            outcome.is_ok(),
            &format!("{origin:?} is admitted for an infrastructure-only peer"),
        );
    }

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
         a trusted peer is legitimate",
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
        strict.ledger.behaviour_originated() > 0 && strict.ledger.allowed_by_origin().is_empty(),
        "an untrusting server's dial-back is made by the crate and REFUSED by its own root \
         gate, so the gate is a real decision point and not a pass-through",
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
        server_pending.iter().all(|n| *n == 1),
        &format!(
            "the dial-back candidate is present at the pending hook, BEFORE any socket \
             opens — so an address check there precedes contact, which one at the \
             established hook would not (got {server_pending:?})"
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
    let mut dest = Node::new(Roles::client(), &[relay_id.clone()], &[]);
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

    {
        let mut nodes = [&mut dest, &mut relay_node, &mut source];
        pump(&mut nodes, Duration::from_secs(12)).await;
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
    // THE RELAY'S OWN LIMITS, recorded because CONNECTIVITY.md §9 sets
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
    // THE CONTROL: identical in every way except the class the relay
    // is authorized under. Built here, beside the subject, so a reader
    // can see that the single difference is the trust set.
    let mut control = ProductionNode::with_trust(&[relay_node.identity.clone()], &[]);

    for target in [&mut node, &mut control] {
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
                .map(|d| (d.connection_id, d.refusal.clone()))
                .collect::<Vec<_>>()
        ),
    );
    report.note(
        "R6.2",
        format!(
            "production gate decisions, control (relay data-plane trusted): {:?}",
            control_decisions
                .iter()
                .map(|d| (d.connection_id, d.refusal.clone()))
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

    // THE CLAIM F1 RESTS ON: the shipped gate refuses the reservation
    // dial toward an infrastructure-only peer, and the refusal names
    // Kademlia — the origin the hook assumed, for a dial no Kademlia
    // made.
    report.require(
        "R6.5",
        subject.iter().all(|d| d.refusal.is_some()) && !subject.is_empty(),
        &format!(
            "every reservation dial toward the infrastructure-only relay was refused: {:?}",
            node.decisions.refusals()
        ),
    );
    report.require(
        "R6.6",
        !node.decisions.refusals().is_empty()
            && node
                .decisions
                .refusals()
                .iter()
                .all(|r| r.contains("kademlia")),
        &format!(
            "the refusal attributes the relay's dial to Kademlia, which is F1 in one string: \
             {:?}",
            node.decisions.refusals()
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
        control.connected > node.connected,
        &format!(
            "control reaches the relay and the subject does not ({} vs {} connection(s))",
            control.connected, node.connected
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
        node.dialing == 0
            && node
                .failures
                .iter()
                .all(|f| !f.contains("Dial error") && !f.contains("Denied")),
        &format!(
            "a gate refusal of a behaviour dial emits no Dialing and no \
             OutgoingConnectionError (dialing {}, swarm events {:?})",
            node.dialing, node.failures
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
        control.dialing == 1 && node.dialing == 0,
        &format!(
            "an ADMITTED behaviour dial is visible as `Dialing` and a REFUSED one is not              (control {} vs subject {})",
            control.dialing, node.dialing
        ),
    );
    report.require(
        "R6.10",
        node.failures
            .iter()
            .any(|f| f.contains("listener closed: Ok")),
        &format!(
            "the only outward trace of the refusal is the relay listener closing \
             SUCCESSFULLY: {:?}",
            node.failures
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

    let mut dest = Node::new(Roles::client(), &[relay_id.clone()], &[]);
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

    {
        let mut nodes = [&mut dest, &mut relay_node, &mut source];
        pump(&mut nodes, Duration::from_secs(12)).await;
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
