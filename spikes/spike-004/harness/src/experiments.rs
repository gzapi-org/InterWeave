// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton

//! The numbered observations. Each returns findings into a [`Report`];
//! the process exits non-zero if any required one is false, so
//! `cargo run` cannot report success while its own output disproves the
//! record.

use std::time::Duration;

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
    client.dial(server_peer_for_dial, server_addr.clone()).expect("dial accepted");

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
}

/// R3 — infrastructure authorization does not reach the data plane.
///
/// ADR-0036's whole point, exercised against the REAL policy rather
/// than a restatement of it: the same peer, the same address, the same
/// instant, admitted for reachability and refused for application
/// traffic.
pub fn r3_infrastructure_cannot_reach_the_data_plane(report: &mut Report) {
    use interweave_transport_api::TransportIdentity;
    use interweave_trust_api::{InfrastructureSet, PeerTrustPolicy};
    use interweave_transport_runtime::{ConnectionManager, TrustSources};

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

    for origin in [
        DialOrigin::RelayReservation,
        DialOrigin::RelayCircuit,
        DialOrigin::AutonatProbe,
        DialOrigin::DcutrHolePunch,
    ] {
        let outcome = ask(origin);
        report.require(
            "R3.1",
            outcome.is_ok(),
            &format!("{origin:?} is admitted for an infrastructure-only peer"),
        );
    }
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
            &format!(
                "{origin:?} is ADMITTED for a peer in both sets — data-plane trust wins"
            ),
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
            n[1]
                .ledger
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
            && strict.ledger.allowed_by_origin().is_empty(),
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
    source.swarm.add_peer_address(relay_peer, relay_addr.clone());

    // WAIT ON THE RELAY, not on the destination. The relay is where a
    // reservation exists; the client's own event can lag it, and the
    // first version dialled the circuit before the relay had the
    // reservation — the relay answered NO_RESERVATION and the run
    // reported a failure that was a race in the fixture.
    {
        let mut nodes = [&mut dest, &mut relay_node, &mut source];
        pump_until(&mut nodes, Duration::from_secs(20), |n| {
            n[1]
                .observed
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
    report.require(
        "R5.6",
        !resolved.contains_key("relay-circuit"),
        "the relay BEHAVIOUR originates no circuit dial — the transport does — so \
         RelayCircuit is a command-path origin that the dialling caller must set",
    );
    report.note(
        "R5.7",
        "the circuit did not complete on loopback (relay reported NO_RESERVATION against a \
         reservation it had accepted). Not claimed as a finding: a relayed data path is \
         phase-A work still to be built, and this run does not distinguish a fixture race \
         from crate behaviour"
            .to_owned(),
    );
}
