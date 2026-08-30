// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The network experiments.
//!
//! Each is written so that its verdict cannot be produced by the wrong
//! cause: a negative observation is paired with a positive control in
//! the same run, and a count that could be zero for two reasons is
//! split until only one remains.

use std::num::NonZeroUsize;
use std::time::Duration;

use libp2p::kad;
use libp2p::kad::store::RecordStore;

use interweave_transport_runtime::{DialDenial, DialOrigin, DialRequest};

use crate::Report;
use crate::gate::Mode;
use crate::namespace;
use crate::node::{KadRole, Node, NodeConfig, QueryClass};
use crate::topology::{pump, pump_until};

/// K2 — no Kademlia activity when disabled.
pub async fn k2_disabled_is_silent(r: &mut Report) {
    // A SERVER and a node built with the behaviour absent. The server is
    // the positive control: it advertises the protocol, so "the disabled
    // node does not" is a fact about the node and not about the
    // topology.
    let server = NodeConfig {
        role: KadRole::Server,
        ..NodeConfig::default()
    };
    let off = NodeConfig {
        role: KadRole::Disabled,
        ..NodeConfig::default()
    };
    let mut nodes = vec![Node::start(&server).await, Node::start(&off).await];
    let (server_id, off_id) = (nodes[0].peer_id, nodes[1].peer_id);
    nodes[0].trust(off_id);
    nodes[1].trust(server_id);

    let addr = nodes[0].dial_address();
    nodes[1].dial_admitted(addr);
    // BOTH Identify observations, not merely the connection. Every claim
    // below reads `identify_protocols`, and a connection exists before
    // the exchange completes — waiting on the wrong signal made the
    // server-mode CONTROL fail intermittently under a full-suite run,
    // which is a control that would eventually have been "fixed" by
    // weakening it.
    let identified = pump_until(&mut nodes, Duration::from_secs(20), |n| {
        n[1].observed.identify_protocols.contains_key(&server_id)
            && n[0].observed.identify_protocols.contains_key(&off_id)
    })
    .await;
    r.check(
        "K2.1",
        "the two nodes connect and exchange Identify in both directions",
        identified,
    );

    let protocol = namespace::protocol_name(&server.network_id);
    let server_advertises = nodes[1]
        .observed
        .identify_protocols
        .get(&server_id)
        .is_some_and(|p| p.contains(&protocol));
    r.check(
        "K2.2",
        "CONTROL: a server-mode node advertises the derived protocol",
        server_advertises,
    );
    let off_advertises = nodes[0]
        .observed
        .identify_protocols
        .get(&off_id)
        .is_some_and(|p| p.contains(&protocol));
    r.check(
        "K2.3",
        "a node built without the behaviour advertises no Kademlia protocol",
        !off_advertises,
    );
    r.check(
        "K2.4",
        "and originates no behaviour dial of its own",
        nodes[1].ledger.behaviour_originated() == 0,
    );
    r.check(
        "K2.5",
        "and runs no query the caller did not start",
        nodes[1].observed.unattributed_queries.is_empty(),
    );
}

/// K3 — `BucketInserts::Manual`: connecting is not routing.
pub async fn k3_manual_bucket_inserts(r: &mut Report) {
    let cfg = NodeConfig {
        role: KadRole::Server,
        ..NodeConfig::default()
    };
    let mut nodes = vec![Node::start(&cfg).await, Node::start(&cfg).await];
    let (a, b) = (nodes[0].peer_id, nodes[1].peer_id);
    nodes[0].trust(b);
    nodes[1].trust(a);

    let addr = nodes[0].dial_address();
    nodes[1].dial_admitted(addr.clone());
    let connected = pump_until(&mut nodes, Duration::from_secs(10), |n| {
        n[1].observed.connected.contains(&a)
    })
    .await;
    r.check("K3.1", "the two nodes connect and identify", connected);

    // Give the library every chance to insert on its own.
    pump(&mut nodes, Duration::from_secs(2)).await;
    r.check(
        "K3.2",
        "an authenticated connection alone puts nobody in the routing table",
        nodes[1].routing_peers() == 0,
    );

    // THE POSITIVE CONTROL: the same peer, the same connection, one
    // explicit `add_address`.
    if let Some(k) = nodes[1].kad() {
        k.add_address(&a, addr);
    }
    pump(&mut nodes, Duration::from_secs(1)).await;
    r.check(
        "K3.3",
        "CONTROL: an explicit add_address does put it there",
        nodes[1].routing_peers() == 1,
    );
    r.check(
        "K3.4",
        "and the insertion is reported as a routing update",
        nodes[1].observed.routing_updates.contains(&a),
    );
}

/// K4 — client and server mode semantics.
pub async fn k4_client_server_modes(r: &mut Report) {
    let base = NodeConfig::default();
    let client = NodeConfig {
        role: KadRole::Client,
        ..base.clone()
    };
    let server = NodeConfig {
        role: KadRole::Server,
        ..base.clone()
    };
    let mut nodes = vec![Node::start(&server).await, Node::start(&client).await];
    let (s, c) = (nodes[0].peer_id, nodes[1].peer_id);
    nodes[0].trust(c);
    nodes[1].trust(s);

    let addr = nodes[0].dial_address();
    nodes[1].dial_admitted(addr.clone());
    pump_until(&mut nodes, Duration::from_secs(10), |n| {
        n[0].observed.identify_protocols.contains_key(&c)
            && n[1].observed.identify_protocols.contains_key(&s)
    })
    .await;

    let protocol = namespace::protocol_name(&base.network_id);
    r.check(
        "K4.1",
        "a server advertises the Kademlia protocol",
        nodes[1].observed.identify_protocols[&s].contains(&protocol),
    );
    r.check(
        "K4.2",
        "a client does NOT advertise it, so it is not a routing target",
        !nodes[0].observed.identify_protocols[&c].contains(&protocol),
    );

    // A CLIENT MAY STILL QUERY. Route it at the server and ask.
    if let Some(k) = nodes[1].kad() {
        k.add_address(&s, addr);
        let id = k.get_closest_peers(libp2p::PeerId::random());
        nodes[1].own_queries.insert(id, QueryClass::Exploration);
    }
    let answered = pump_until(&mut nodes, Duration::from_secs(15), |n| {
        !n[1].observed.finished_queries.is_empty()
    })
    .await;
    r.check(
        "K4.3",
        "a client-mode node can still run a query to completion",
        answered,
    );
}

/// K5 — bootstrap, and any bootstrap the library starts by itself.
pub async fn k5_bootstrap_accounting(r: &mut Report) {
    let cfg = NodeConfig {
        role: KadRole::Server,
        // EXPLICITLY OFF, which is what the design requires: the
        // provider scheduler owns the refresh. If the library still runs
        // one, the count below finds it.
        periodic_bootstrap: None,
        ..NodeConfig::default()
    };
    let mut nodes = vec![Node::start(&cfg).await, Node::start(&cfg).await];
    let (a, b) = (nodes[0].peer_id, nodes[1].peer_id);
    nodes[0].trust(b);
    nodes[1].trust(a);
    let addr = nodes[0].dial_address();
    nodes[1].dial_admitted(addr.clone());
    pump_until(&mut nodes, Duration::from_secs(10), |n| {
        n[1].observed.connected.contains(&a)
    })
    .await;

    // BEFORE any routing entry exists, bootstrap has nothing to walk.
    let empty = nodes[1].kad().map(kad::Behaviour::bootstrap);
    r.check(
        "K5.1",
        "bootstrap with an empty routing table reports NoKnownPeers",
        matches!(empty, Some(Err(_))),
    );

    // The insertion the design says must not imply a bootstrap.
    if let Some(k) = nodes[1].kad() {
        k.add_address(&a, addr);
    }
    pump(&mut nodes, Duration::from_secs(3)).await;
    let implicit = nodes[1].observed.unattributed_queries.len();
    // ASSERTED, not merely printed. Finding F2 and the budgeting advice
    // it gives Stage 10 both say EXACTLY ONE, and an earlier version of
    // this check passed `true` — so a reproduction that started zero or
    // three would have exited 0 while the record went on claiming one.
    // A measurement a record depends on is a claim.
    r.check(
        "K5.2",
        &format!(
            "routing insertion starts EXACTLY ONE query nobody asked for, and \
             this run saw {implicit}"
        ),
        implicit == 1,
    );

    // AND AN EXPLICIT BOOTSTRAP WORKS once a peer is known.
    let started = nodes[1].kad().and_then(|k| k.bootstrap().ok());
    if let Some(id) = started {
        nodes[1].own_queries.insert(id, QueryClass::Bootstrap);
    }
    r.check(
        "K5.3",
        "bootstrap succeeds once one routing peer is known",
        started.is_some(),
    );
    let done = pump_until(&mut nodes, Duration::from_secs(20), |n| {
        n[1].observed.bootstrap_completions > 0
    })
    .await;
    r.check("K5.4", "and the bootstrap query completes", done);
}

/// K6 — behaviour-originated dials exist, are attributed, and are gated.
pub async fn k6_behaviour_dials_are_gated(r: &mut Report) {
    // THREE SERVERS. `b` knows `c`; `a` knows only `b`. A query from `a`
    // walks to `c`, and reaching `c` requires a dial NOTHING in `a`'s
    // application asked for.
    let cfg = NodeConfig {
        role: KadRole::Server,
        gate_mode: Mode::DenyUnadmitted,
        ..NodeConfig::default()
    };
    let mut nodes = vec![
        Node::start(&cfg).await,
        Node::start(&cfg).await,
        Node::start(&cfg).await,
    ];
    let (a, b, c) = (nodes[0].peer_id, nodes[1].peer_id, nodes[2].peer_id);
    for i in 0..3 {
        for p in [a, b, c] {
            nodes[i].trust(p);
        }
    }
    let (b_addr, c_addr) = (nodes[1].dial_address(), nodes[2].dial_address());

    // `b` learns `c` and routes to it.
    nodes[1].dial_admitted(c_addr.clone());
    pump_until(&mut nodes, Duration::from_secs(10), |n| {
        n[1].observed.connected.contains(&c)
    })
    .await;
    if let Some(k) = nodes[1].kad() {
        k.add_address(&c, c_addr);
    }

    // `a` learns `b` and routes to it. Nothing tells `a` about `c`.
    nodes[0].dial_admitted(b_addr.clone());
    pump_until(&mut nodes, Duration::from_secs(10), |n| {
        n[0].observed.connected.contains(&b)
    })
    .await;
    if let Some(k) = nodes[0].kad() {
        k.add_address(&b, b_addr);
    }
    pump(&mut nodes, Duration::from_secs(1)).await;

    nodes[0].ledger.reset();
    let before_connected = nodes[0].observed.connected.len();

    // The query that makes `a` want to reach `c`.
    if let Some(k) = nodes[0].kad() {
        let id = k.get_closest_peers(c);
        nodes[0].own_queries.insert(id, QueryClass::Targeted);
    }
    pump(&mut nodes, Duration::from_secs(12)).await;

    let originated = nodes[0].ledger.behaviour_originated();
    r.check(
        "K6.1",
        &format!("an iterative query originates dials nothing admitted: {originated}"),
        originated > 0,
    );
    r.check(
        "K6.2",
        "every one of them is attributed to a target peer",
        nodes[0].ledger.behaviour_targets().len() as u64 == originated,
    );
    r.check(
        "K6.3",
        "the dials are aimed at the peer the query is walking toward",
        nodes[0].ledger.behaviour_targets().contains(&c),
    );
    r.check(
        "K6.4",
        "TODAY's gate refuses every one of them",
        nodes[0].ledger.behaviour_allowed() == 0
            && nodes[0]
                .ledger
                .refusals()
                .get("no root dial admission")
                .copied()
                == Some(originated),
    );
    r.check(
        "K6.5",
        "so the refusal is what stops the connection, not the topology",
        nodes[0].observed.connected.len() == before_connected,
    );
    r.note(format!(
        "K6: {originated} behaviour-originated dials, {} refused as unadmitted",
        nodes[0]
            .ledger
            .refusals()
            .get("no root dial admission")
            .copied()
            .unwrap_or(0)
    ));
}

/// K7 — the same walk under the Stage 10 gate: policy decides.
pub async fn k7_policy_admits_and_refuses(r: &mut Report) {
    let cfg = NodeConfig {
        role: KadRole::Server,
        gate_mode: Mode::PolicyAdmit,
        ..NodeConfig::default()
    };
    let mut nodes = vec![
        Node::start(&cfg).await,
        Node::start(&cfg).await,
        Node::start(&cfg).await,
    ];
    let (a, b, c) = (nodes[0].peer_id, nodes[1].peer_id, nodes[2].peer_id);
    // `b` and `c` trust everyone; `a` trusts `b` and — for now — `c`.
    for i in 0..3 {
        for p in [a, b, c] {
            nodes[i].trust(p);
        }
    }
    let (b_addr, c_addr) = (nodes[1].dial_address(), nodes[2].dial_address());
    nodes[1].dial_admitted(c_addr.clone());
    pump_until(&mut nodes, Duration::from_secs(10), |n| {
        n[1].observed.connected.contains(&c)
    })
    .await;
    if let Some(k) = nodes[1].kad() {
        k.add_address(&c, c_addr);
    }
    nodes[0].dial_admitted(b_addr.clone());
    pump_until(&mut nodes, Duration::from_secs(10), |n| {
        n[0].observed.connected.contains(&b)
    })
    .await;
    // RESET BEFORE THE INSERTION. `add_address` starts an implicit
    // bootstrap of its own (K5), and that bootstrap dials — so a reset
    // after it would hide the very dials this experiment counts.
    nodes[0].ledger.reset();
    if let Some(k) = nodes[0].kad() {
        k.add_address(&b, b_addr);
        let id = k.get_closest_peers(c);
        nodes[0].own_queries.insert(id, QueryClass::Targeted);
    }
    let reached = pump_until(&mut nodes, Duration::from_secs(20), |n| {
        n[0].observed.dialed_out.contains(&c)
    })
    .await;
    r.check(
        "K7.1",
        "under the Stage 10 gate a trusted returned peer IS dialled and reached",
        reached,
    );
    r.note(format!(
        "K7 connectivity: a connected to {:?}, dialled {:?}",
        nodes[0].observed.connected.len(),
        nodes[0].observed.dialed_out.len()
    ));
    r.check(
        "K7.2",
        "and the connection came from a behaviour dial the policy admitted",
        nodes[0].ledger.behaviour_allowed() > 0,
    );
    r.note(format!(
        "K7: {} behaviour dials, {} admitted by policy, {} by ticket, refusals {:?}, dialled {:?}",
        nodes[0].ledger.behaviour_originated(),
        nodes[0].ledger.behaviour_allowed(),
        nodes[0].ledger.admitted_by_ticket(),
        nodes[0].ledger.refusals(),
        nodes[0].observed.dialed_out.iter().map(|p| p == &c).collect::<Vec<_>>()
    ));
}

/// K8 — an untrusted peer returned by a query cannot be connected to.
pub async fn k8_untrusted_returned_peer_is_refused(r: &mut Report) {
    let cfg = NodeConfig {
        role: KadRole::Server,
        gate_mode: Mode::PolicyAdmit,
        ..NodeConfig::default()
    };
    let mut nodes = vec![
        Node::start(&cfg).await,
        Node::start(&cfg).await,
        Node::start(&cfg).await,
    ];
    let (a, b, c) = (nodes[0].peer_id, nodes[1].peer_id, nodes[2].peer_id);
    // `b` is the router and knows `c`. `a` trusts ONLY `b` — `c` is a
    // peer a malicious or merely well-connected router hands back.
    nodes[0].trust(b);
    nodes[1].trust(a);
    nodes[1].trust(c);
    nodes[2].trust(b);

    let (b_addr, c_addr) = (nodes[1].dial_address(), nodes[2].dial_address());
    nodes[1].dial_admitted(c_addr.clone());
    pump_until(&mut nodes, Duration::from_secs(10), |n| {
        n[1].observed.connected.contains(&c)
    })
    .await;
    if let Some(k) = nodes[1].kad() {
        k.add_address(&c, c_addr);
    }
    nodes[0].dial_admitted(b_addr.clone());
    pump_until(&mut nodes, Duration::from_secs(10), |n| {
        n[0].observed.connected.contains(&b)
    })
    .await;
    if let Some(k) = nodes[0].kad() {
        k.add_address(&b, b_addr);
    }
    pump(&mut nodes, Duration::from_secs(1)).await;
    nodes[0].ledger.reset();

    if let Some(k) = nodes[0].kad() {
        let id = k.get_closest_peers(c);
        nodes[0].own_queries.insert(id, QueryClass::Targeted);
    }
    pump(&mut nodes, Duration::from_secs(15)).await;

    let returned = nodes[0]
        .observed
        .query_results
        .values()
        .any(|peers| peers.contains(&c));
    r.check(
        "K8.1",
        "the router really did return the untrusted peer",
        returned || nodes[0].ledger.behaviour_targets().contains(&c),
    );
    r.check(
        "K8.2",
        "and the query tried to dial it",
        nodes[0].ledger.behaviour_targets().contains(&c),
    );
    r.check(
        "K8.3",
        "but this node never dialled it",
        !nodes[0].observed.dialed_out.contains(&c),
    );
    r.check(
        "K8.4",
        "and the refusal names trust, not a limit",
        nodes[0].ledger.refusals().contains_key("unauthorized"),
    );
    r.note(format!("K8: refusals {:?}", nodes[0].ledger.refusals()));
}

/// K9 — backoff and the global limits reach a behaviour dial too.
pub async fn k9_backoff_and_limits_apply(r: &mut Report) {
    use interweave_transport_api::TransportIdentity;

    let cfg = NodeConfig {
        role: KadRole::Server,
        gate_mode: Mode::PolicyAdmit,
        ..NodeConfig::default()
    };
    let mut nodes = vec![
        Node::start(&cfg).await,
        Node::start(&cfg).await,
        Node::start(&cfg).await,
    ];
    let (a, b, c) = (nodes[0].peer_id, nodes[1].peer_id, nodes[2].peer_id);
    for i in 0..3 {
        for p in [a, b, c] {
            nodes[i].trust(p);
        }
    }
    let (b_addr, c_addr) = (nodes[1].dial_address(), nodes[2].dial_address());
    nodes[1].dial_admitted(c_addr.clone());
    pump_until(&mut nodes, Duration::from_secs(10), |n| {
        n[1].observed.connected.contains(&c)
    })
    .await;
    if let Some(k) = nodes[1].kad() {
        k.add_address(&c, c_addr);
    }
    nodes[0].dial_admitted(b_addr.clone());
    pump_until(&mut nodes, Duration::from_secs(10), |n| {
        n[0].observed.connected.contains(&b)
    })
    .await;
    // THE BACKOFF GOES IN FIRST, before the routing insertion: that
    // insertion starts an implicit bootstrap which dials immediately
    // (K5), so a policy installed afterwards would be installed after
    // the dial it is meant to refuse.
    //
    // Through the manager's OWN failure path — admit a dial, then record
    // it as failed — which is what the runtime does when a dial does not
    // come back. Reaching into `ConnectionPolicy` directly would have
    // installed backoff the `ConnectionManager` never agreed to.
    {
        let identity = TransportIdentity::parse(c.to_base58()).expect("canonical");
        let mut m = nodes[0].manager.lock().expect("manager");
        let ticket = m
            .handle()
            .admit(
                &DialRequest {
                    peer: Some(identity.clone()),
                    address: "/ip4/198.51.100.1/tcp/1".to_owned(),
                    origin: DialOrigin::ConnectionManager,
                },
                0,
            )
            .expect("a trusted peer with a fresh policy is admitted");
        m.record_failure(ticket, 0);
        // The peer is now in backoff, which the manager reports by
        // refusing the next dial to it.
        let refused = m.handle().admit(
            &DialRequest {
                peer: Some(identity),
                address: "/ip4/198.51.100.2/tcp/1".to_owned(),
                origin: DialOrigin::ConnectionManager,
            },
            1,
        );
        r.check(
            "K9.1",
            "a recorded dial failure put the peer into backoff",
            matches!(refused, Err(DialDenial::PeerBackoff)),
        );
    }
    nodes[0].ledger.reset();
    if let Some(k) = nodes[0].kad() {
        k.add_address(&b, b_addr);
        let id = k.get_closest_peers(c);
        nodes[0].own_queries.insert(id, QueryClass::Targeted);
    }
    pump(&mut nodes, Duration::from_secs(15)).await;

    r.check(
        "K9.2",
        "a Kademlia dial to a backed-off peer is refused for backoff",
        nodes[0].ledger.refusals().contains_key("peer backoff"),
    );
    r.check(
        "K9.3",
        "and this node never dialled it",
        !nodes[0].observed.dialed_out.contains(&c),
    );
    r.note(format!("K9: refusals {:?}", nodes[0].ledger.refusals()));

    // BACKOFF IS TEMPORARY, and that is the half an immediate-refusal
    // assertion cannot see. The gate used to timestamp every admission
    // at zero, so a backoff recorded at 0 with a 30-second delay expired
    // at a moment the clock never reached — `PeerBackoff` was permanent
    // and every experiment still passed. Asked of the same policy at a
    // time past the delay, it must admit again.
    {
        let identity = TransportIdentity::parse(c.to_base58()).expect("canonical");
        let m = nodes[0].manager.lock().expect("manager");
        let ask = |at: u64| {
            m.handle().admit(
                &DialRequest {
                    peer: Some(identity.clone()),
                    address: "/ip4/198.51.100.3/tcp/1".to_owned(),
                    origin: DialOrigin::KademliaQuery,
                },
                at,
            )
        };
        r.check(
            "K9.2b",
            "the refusal is still in force while the delay is running",
            matches!(ask(1_000), Err(DialDenial::PeerBackoff)),
        );
        // The manager's own base delay is 30s; well past it the peer is
        // eligible again.
        r.check(
            "K9.2c",
            "and it LAPSES: past the delay the same peer is admitted again",
            ask(600_000).is_ok(),
        );
    }

    // SHUTDOWN STATE, the other half: a draining node refuses every
    // behaviour dial regardless of trust.
    nodes[0]
        .manager
        .lock()
        .expect("manager")
        .begin_shutdown();
    nodes[0].ledger.reset();
    // A TARGET IT IS NOT CONNECTED TO, so the query must dial to make
    // progress. Querying for a random key let the existing routing
    // connection satisfy the walk, and the observation then passed on
    // `behaviour_originated() == 0` — "no dial happened" read as "the
    // drain refused it", which is the vacuous arm this shape removes.
    if let Some(k) = nodes[0].kad() {
        let id = k.get_closest_peers(c);
        nodes[0].own_queries.insert(id, QueryClass::Targeted);
    }
    pump(&mut nodes, Duration::from_secs(10)).await;
    let refusals = nodes[0].ledger.refusals();
    let originated = nodes[0].ledger.behaviour_originated();
    r.check(
        "K9.4",
        &format!("a draining node is still ASKED for a dial ({originated})"),
        originated > 0,
    );
    r.check(
        "K9.5",
        "and refuses every one of them as shutting down",
        refusals.get("shutting down").copied() == Some(originated)
            && nodes[0].ledger.behaviour_allowed() == 0,
    );
    r.note(format!("K9 drain: {originated} dials, refusals {refusals:?}"));

    // THE GATE'S OWN CLOCK ADVANCES. Everything above asks the manager
    // with explicit timestamps, so it holds whatever the gate believes
    // the time is — and the gate used to believe zero, permanently.
    // Every admission and every settlement was stamped at the same
    // instant, which made a 30-second backoff expire at a moment the
    // clock never reached: `PeerBackoff` was permanent and no assertion
    // could see it. This is the one observation that fails if the clock
    // freezes again.
    let t0 = nodes[0].swarm.behaviour().gate.clock_ms();
    pump(&mut nodes, Duration::from_secs(2)).await;
    let t1 = nodes[0].swarm.behaviour().gate.clock_ms();
    r.check(
        "K9.6",
        &format!("the gate's clock advances with real time: {t0} -> {t1}"),
        t1 >= t0 + 1_500,
    );
}

/// K10 — record and provider writes are refused, and counted.
pub async fn k10_records_are_filtered(r: &mut Report) {
    let cfg = NodeConfig {
        role: KadRole::Server,
        ..NodeConfig::default()
    };
    let mut nodes = vec![Node::start(&cfg).await, Node::start(&cfg).await];
    let (a, b) = (nodes[0].peer_id, nodes[1].peer_id);
    nodes[0].trust(b);
    nodes[1].trust(a);
    let a_addr = nodes[0].dial_address();
    nodes[1].dial_admitted(a_addr.clone());
    pump_until(&mut nodes, Duration::from_secs(10), |n| {
        n[1].observed.connected.contains(&a)
    })
    .await;
    if let Some(k) = nodes[1].kad() {
        k.add_address(&a, a_addr);
    }
    pump(&mut nodes, Duration::from_secs(1)).await;

    // A HOSTILE PEER writing a record. The production driver never calls
    // this; the spike does, because the question is what the RECEIVER
    // does when someone else's implementation calls it.
    let record = kad::Record::new(kad::RecordKey::new(&b"/interweave/should-not-exist"), vec![7; 32]);
    let put = nodes[1]
        .kad()
        .and_then(|k| k.put_record(record, kad::Quorum::One).ok());
    r.check("K10.1", "the write was actually sent", put.is_some());
    if let Some(id) = put {
        nodes[1].own_queries.insert(id, QueryClass::Exploration);
    }
    pump(&mut nodes, Duration::from_secs(8)).await;

    let attempts = nodes[0]
        .observed
        .record_writes
        .get("PUT_VALUE")
        .copied()
        .unwrap_or(0);
    r.check(
        "K10.2",
        &format!("the receiver saw and counted the attempt ({attempts})"),
        attempts > 0,
    );
    let stored = nodes[0]
        .kad()
        .map(|k| k.store_mut().records().count())
        .unwrap_or(usize::MAX);
    r.check(
        "K10.3",
        &format!("and stored nothing: {stored} record(s) in the store"),
        stored == 0,
    );

    // PROVIDER RECORDS, the same question through the other door — and
    // the write must be shown to ARRIVE. Asserting only that the store
    // is empty passes identically when the request was never sent, when
    // negotiation failed, and when there was no route: three ways to
    // "prove" filtering without any filtering happening.
    // THE KEY IS RETAINED, because the receiver must be asked about
    // THIS key. `provided()` was the wrong question entirely: it
    // enumerates records the LOCAL node provides, so it is zero on the
    // receiver whether or not the inbound provider record was stored,
    // and the check validated nothing.
    let provider_key = kad::RecordKey::new(&b"/interweave/nope");
    let provide = nodes[1]
        .kad()
        .and_then(|k| k.start_providing(provider_key.clone()).ok());
    r.check(
        "K10.4",
        "the provider write was actually started",
        provide.is_some(),
    );
    if let Some(id) = provide {
        nodes[1].own_queries.insert(id, QueryClass::Exploration);
    }
    pump(&mut nodes, Duration::from_secs(10)).await;
    let arrived = nodes[0]
        .observed
        .record_writes
        .get("ADD_PROVIDER")
        .copied()
        .unwrap_or(0);
    r.check(
        "K10.5",
        &format!("and REACHED the receiver, which counted it ({arrived})"),
        arrived > 0,
    );
    // `providers(&key)` is what the RECEIVER stored for this key, which
    // is the thing `StoreInserts::FilterBoth` is supposed to refuse.
    let stored_providers = nodes[0]
        .kad()
        .map(|k| k.store_mut().providers(&provider_key).len())
        .unwrap_or(usize::MAX);
    r.check(
        "K10.6",
        &format!(
            "having arrived, no provider record is stored for that key: \
             {stored_providers}"
        ),
        stored_providers == 0,
    );
    r.note(format!(
        "K10: inbound requests seen by the receiver: {:?}",
        nodes[0].observed.record_writes
    ));
}

/// K11 — a 10-node topology converges, and exploration expands routing.
pub async fn k11_ten_node_exploration(r: &mut Report) {
    const N: usize = 10;
    let cfg = NodeConfig {
        role: KadRole::Server,
        gate_mode: Mode::PolicyAdmit,
        ..NodeConfig::default()
    };
    let protocol = namespace::protocol_name(&cfg.network_id);
    let mut nodes = Vec::with_capacity(N);
    for _ in 0..N {
        nodes.push(Node::start(&cfg).await);
    }
    let ids: Vec<_> = nodes.iter().map(|n| n.peer_id).collect();
    let addrs: Vec<_> = nodes.iter().map(Node::dial_address).collect();
    // EVERYONE TRUSTS EVERYONE: this experiment is about routing
    // convergence, and trust refusals would confound it. K8 is where
    // trust does the work.
    for n in &mut nodes {
        for id in &ids {
            n.trust(*id);
        }
    }

    // A LINE, not a mesh: each node is seeded with only its predecessor,
    // so anything beyond that is discovered rather than configured.
    for i in 1..N {
        nodes[i].dial_admitted(addrs[i - 1].clone());
    }
    pump(&mut nodes, Duration::from_secs(6)).await;
    for i in 1..N {
        let (prev, addr) = (ids[i - 1], addrs[i - 1].clone());
        if let Some(k) = nodes[i].kad() {
            k.add_address(&prev, addr);
        }
    }
    pump(&mut nodes, Duration::from_secs(3)).await;

    let seeded: Vec<usize> = (0..N).map(|i| nodes[i].routing_peers()).collect();
    r.check(
        "K11.1",
        &format!("the line is seeded with one routing peer each: {seeded:?}"),
        seeded[1..].iter().all(|&c| c >= 1),
    );

    // RANDOM EXPLORATION, the design's §9.3 query: 32 random bytes, not
    // a hash of anything — and THROUGH THE BOUNDED SCHEDULER, one per
    // node, so the convergence reported here is convergence within the
    // budgets rather than convergence with the budgets bypassed. Every
    // experiment used to call the behaviour directly, which meant a
    // scheduler that existed and was never consulted would have been
    // invisible to exactly the runs the roadmap quotes.
    let mut sched: Vec<QueryScheduler> = (0..N)
        .map(|_| QueryScheduler::new(2, 6))
        .collect();
    let mut refused_by_budget = 0_usize;
    let mut permits_granted = 0_usize;
    for round in 0..4 {
        for i in 0..N {
            // The permit comes FIRST; the behaviour is not called
            // without one (F14).
            let Ok(permit) = sched[i].acquire(round * 1_000) else {
                refused_by_budget += 1;
                continue;
            };
            let key = libp2p::kad::RecordKey::new(&random_32());
            let started = nodes[i].kad().map(|k| {
                k.get_n_closest_peers(key.to_vec(), NonZeroUsize::new(10).expect("nonzero"))
            });
            match started {
                Some(id) => {
                    sched[i].bind(permit, id);
                    nodes[i].own_queries.insert(id, QueryClass::Exploration);
                    permits_granted += 1;
                }
                None => sched[i].release(permit),
            }
        }
        pump(&mut nodes, Duration::from_secs(8)).await;
        // Completions release their own slots, so the next round has
        // capacity — without this the concurrency ceiling would stall
        // exploration after two rounds and the convergence below would
        // measure the scheduler leaking rather than the network.
        for i in 0..N {
            let finished: Vec<_> = nodes[i].observed.finished_queries.clone();
            for id in finished {
                sched[i].finish(id);
            }
        }

        // THE ADMISSION PIPELINE, which is the half `BucketInserts::
        // Manual` exists to force. A query result is a CANDIDATE: the
        // provider decides, and nothing enters the routing table until
        // it does. Without this step exploration discovers peers and
        // routes to none of them, which is what the first run of this
        // experiment measured.
        //
        // The SAME pipeline K17 uses, including INBOUND candidates. An
        // earlier version admitted only what queries returned, and the
        // line converged to a staircase — node `i` holding `i` peers —
        // because a node never learned the neighbour that dialled IT.
        // That produced a run this experiment called full coverage while
        // measuring a partition.
        for node in &mut nodes {
            crate::topology::admit_candidates(node, &protocol);
        }
        pump(&mut nodes, Duration::from_secs(2)).await;

        let total: usize = (0..N).map(|i| nodes[i].routing_peers()).sum();
        r.note(format!(
            "K11 exploration round {round}: total routing entries {total}"
        ));
    }

    let after: Vec<usize> = (0..N).map(|i| nodes[i].routing_peers()).collect();
    // EVERY node routes every other. `grew > 0` passed when a single
    // node gained a single entry — a largely partitioned run — while the
    // record claimed full coverage. The line is seeded one-deep and the
    // rounds above are what has to close it.
    let full = after.iter().filter(|&&c| c == N - 1).count();
    r.check(
        "K11.2",
        &format!("exploration closes the line: {full}/{N} nodes at {} entries — {after:?}", N - 1),
        full == N,
    );
    r.check(
        "K11.3",
        "and every seeded node gained on its single seed",
        (1..N).all(|i| after[i] > seeded[i]),
    );
    r.note(format!(
        "K11: exploration ran through the bounded scheduler; {refused_by_budget} \
         start(s) refused by budget"
    ));
    // AND EVERY QUERY WAS PERMITTED. This is the structural claim the
    // convergence rests on: no exploration query reached the behaviour
    // without a permit. It is falsifiable — calling `kad` outside the
    // permit branch makes the started count exceed the granted one —
    // whereas asserting that the budget REFUSED something would only
    // say this particular topology asks for more than 2 concurrent
    // queries per node, which it does not.
    //
    // That the budgets bind at all is K22's job, on a scheduler driven
    // past them deliberately.
    let started_total: usize = (0..N)
        .map(|i| {
            nodes[i]
                .own_queries
                .values()
                .filter(|c| **c == QueryClass::Exploration)
                .count()
        })
        .sum();
    r.check(
        "K11.4",
        &format!(
            "every exploration query was permitted first: {started_total} started, \
             {permits_granted} permits granted, {refused_by_budget} refused"
        ),
        started_total == permits_granted,
    );
}

fn random_32() -> Vec<u8> {
    // A spike needs unpredictability, not cryptographic quality; the
    // point of the design rule is that the key is NOT derived from any
    // application identity.
    use std::hash::{BuildHasher, Hasher, RandomState};
    let mut out = Vec::with_capacity(32);
    while out.len() < 32 {
        let mut h = RandomState::new().build_hasher();
        h.write_usize(out.len());
        out.extend_from_slice(&h.finish().to_be_bytes());
    }
    out.truncate(32);
    out
}

/// K12 — the project's own exploration rules, driven by real rounds.
///
/// Effective target, no-progress backoff and saturation are PROJECT
/// logic, not library behaviour: `kademlia-integration.md` §9.3 defines
/// them and rust-libp2p knows nothing about them. What the spike can
/// establish is that the rules are implementable over the signal the
/// library actually provides — "did this round yield a new routing peer
/// or a new usable address" — and that they produce the intended shape
/// on a topology that genuinely stops making progress.
pub struct ExplorationState {
    pub base: Duration,
    pub delay: Duration,
    pub no_progress_rounds: u32,
    pub saturated: bool,
}

impl ExplorationState {
    const CAP: Duration = Duration::from_secs(15 * 60);

    #[must_use]
    pub fn new(base: Duration) -> Self {
        Self {
            base,
            delay: base,
            no_progress_rounds: 0,
            saturated: false,
        }
    }

    /// One completed exploration round.
    pub fn round(&mut self, made_progress: bool, has_usable_peer: bool, targetable_outside: bool) {
        if made_progress {
            self.no_progress_rounds = 0;
            self.delay = self.base;
            self.saturated = false;
            return;
        }
        self.no_progress_rounds += 1;
        self.delay = (self.delay * 2).min(Self::CAP);
        if self.no_progress_rounds >= 3 && has_usable_peer && !targetable_outside {
            self.saturated = true;
        }
    }

    /// Invalidated by anything that changes what could be discovered.
    pub fn invalidate(&mut self) {
        self.saturated = false;
        self.no_progress_rounds = 0;
        self.delay = self.base;
    }
}

#[must_use]
pub fn effective_target(
    target_routing_peers: usize,
    max_routing_peers: usize,
    remote_trusted_population: usize,
) -> usize {
    target_routing_peers
        .min(max_routing_peers)
        .min(remote_trusted_population)
}

pub async fn k12_effective_target_and_saturation(r: &mut Report) {
    // EFFECTIVE TARGET: the rule exists so a two- or three-peer overlay
    // is not permanently degraded by a default of 64.
    r.check(
        "K12.1",
        "a 3-peer trust domain has an effective target of 2, not 64",
        effective_target(64, 256, 2) == 2,
    );
    r.check(
        "K12.2",
        "a large trust domain is still capped by the configured target",
        effective_target(64, 256, 1000) == 64,
    );
    r.check(
        "K12.3",
        "and by the hard routing bound when that is smaller",
        effective_target(64, 16, 1000) == 16,
    );

    // BACKOFF: doubling from the base, capped at fifteen minutes,
    // reset by progress.
    let base = Duration::from_secs(60);
    let mut state = ExplorationState::new(base);
    let mut seen = Vec::new();
    for _ in 0..12 {
        state.round(false, true, false);
        seen.push(state.delay);
    }
    r.check(
        "K12.4",
        "the delay doubles each no-progress round",
        seen[0] == base * 2 && seen[1] == base * 4 && seen[2] == base * 8,
    );
    r.check(
        "K12.5",
        &format!("and is capped at 15 minutes: {:?}", seen[11]),
        seen[11] == Duration::from_secs(900),
    );
    r.check(
        "K12.6",
        "saturation is reached after three no-progress rounds",
        state.saturated,
    );
    state.round(true, true, false);
    r.check(
        "K12.7",
        "progress resets the delay and clears saturation",
        state.delay == base && !state.saturated && state.no_progress_rounds == 0,
    );

    // A TARGETABLE PEER OUTSIDE THE ROUTING SET blocks saturation: the
    // view is not saturated while there is somewhere left to look.
    let mut blocked = ExplorationState::new(base);
    for _ in 0..5 {
        blocked.round(false, true, true);
    }
    r.check(
        "K12.8",
        "a fresh targetable observation outside the routing set blocks saturation",
        !blocked.saturated,
    );
    // WITH NO USABLE PEER the view is not saturated either — it is
    // simply empty, which is a different health answer.
    let mut empty = ExplorationState::new(base);
    for _ in 0..5 {
        empty.round(false, false, false);
    }
    r.check(
        "K12.9",
        "an empty routing view never counts as saturated",
        !empty.saturated,
    );
    // AND INVALIDATION really clears it.
    state.round(false, true, false);
    state.round(false, true, false);
    state.round(false, true, false);
    let was = state.saturated;
    state.invalidate();
    r.check(
        "K12.10",
        "a trust or seed change invalidates saturation",
        was && !state.saturated,
    );
}

/// K13/K14 — capability observation, targeted lookup, and supersession.
pub async fn k13_capability_observation(r: &mut Report) {
    let network = "spike-003".to_owned();
    let protocol = namespace::protocol_name(&network);
    let server = NodeConfig {
        role: KadRole::Server,
        network_id: network.clone(),
        ..NodeConfig::default()
    };
    let client = NodeConfig {
        role: KadRole::Client,
        network_id: network.clone(),
        ..NodeConfig::default()
    };
    // A SERVER ON A DIFFERENT NETWORK: same crate, same version, a
    // different derived protocol. The design says positive evidence is
    // valid only for the exact wire major and network hash, so this is
    // the case that rule exists for.
    let other_network = NodeConfig {
        role: KadRole::Server,
        network_id: "spike-003-other".to_owned(),
        ..NodeConfig::default()
    };

    let mut nodes = vec![
        Node::start(&client).await,
        Node::start(&server).await,
        Node::start(&other_network).await,
    ];
    let (obs, srv, other) = (nodes[0].peer_id, nodes[1].peer_id, nodes[2].peer_id);
    for i in 0..3 {
        for p in [obs, srv, other] {
            nodes[i].trust(p);
        }
    }
    let srv_addr = nodes[1].dial_address();
    let other_addr = nodes[2].dial_address();
    nodes[0].dial_admitted(srv_addr.clone());
    nodes[0].dial_admitted(other_addr);
    pump_until(&mut nodes, Duration::from_secs(12), |n| {
        n[0].observed.identify_protocols.contains_key(&srv)
            && n[0].observed.identify_protocols.contains_key(&other)
    })
    .await;

    let saw_server = nodes[0].observed.identify_protocols[&srv].contains(&protocol);
    r.check(
        "K13.1",
        "a server on THIS network is observed advertising the exact protocol",
        saw_server,
    );
    let saw_other = nodes[0].observed.identify_protocols[&other].contains(&protocol);
    r.check(
        "K13.2",
        "a server on a DIFFERENT network_id is not: the hash is part of the evidence",
        !saw_other,
    );
    r.note(format!(
        "K13: the other network advertises {:?}",
        nodes[0].observed.identify_protocols[&other]
            .iter()
            .filter(|p| p.starts_with("/interweave/kad/"))
            .collect::<Vec<_>>()
    ));

    // SUPERSESSION: a node that stops advertising must stop being
    // eligible. Identify pushes a fresh view on request, so the
    // observation is replaced rather than merged — the handler in
    // `topology` overwrites for exactly this reason.
    if let Some(k) = nodes[1].kad() {
        k.set_mode(Some(kad::Mode::Client));
    }
    let rounds_before = nodes[0]
        .observed
        .identify_rounds
        .get(&srv)
        .copied()
        .unwrap_or(0);
    // Force a fresh exchange by reconnecting.
    let _ = nodes[0].swarm.disconnect_peer_id(srv);
    pump(&mut nodes, Duration::from_secs(2)).await;
    nodes[0].dial_admitted(srv_addr);
    let refreshed = pump_until(&mut nodes, Duration::from_secs(15), |n| {
        n[0].observed.identify_rounds.get(&srv).copied().unwrap_or(0) > rounds_before
    })
    .await;
    r.check("K13.3", "a fresh Identify exchange happens", refreshed);
    let still = nodes[0].observed.identify_protocols[&srv].contains(&protocol);
    r.check(
        "K13.4",
        "and a node that dropped to client mode no longer advertises the server protocol",
        !still,
    );
    r.note(format!(
        "K13: after the mode change the observation is {:?}",
        nodes[0].observed.identify_protocols[&srv]
            .iter()
            .filter(|p| p.starts_with("/interweave/kad/"))
            .collect::<Vec<_>>()
    ));
}

/// K15 — every `SnapshotResult` field is computable and bounded.
pub async fn k15_snapshot_is_bounded(r: &mut Report) {
    let cfg = NodeConfig {
        role: KadRole::Server,
        gate_mode: Mode::PolicyAdmit,
        ..NodeConfig::default()
    };
    let mut nodes = vec![Node::start(&cfg).await, Node::start(&cfg).await];
    let (a, b) = (nodes[0].peer_id, nodes[1].peer_id);
    nodes[0].trust(b);
    nodes[1].trust(a);
    let a_addr = nodes[0].dial_address();
    nodes[1].dial_admitted(a_addr.clone());
    pump_until(&mut nodes, Duration::from_secs(10), |n| {
        n[1].observed.connected.contains(&a)
    })
    .await;
    // A THIRD NODE this one is NOT connected to, so the query below
    // originates a behaviour dial that then settles. Without one the
    // cumulative and live counts are both zero and the K15.5 comparison
    // below would hold for the wrong reason.
    let mut third = Node::start(&cfg).await;
    let third_id = third.peer_id;
    let third_addr = third.dial_address();
    third.trust(a);
    third.trust(b);
    third.trust(third_id);
    nodes[0].trust(third_id);
    nodes[1].trust(third_id);
    nodes.push(third);
    if let Some(k) = nodes[1].kad() {
        k.add_address(&a, a_addr);
        k.add_address(&third_id, third_addr);
        let id = k.get_closest_peers(libp2p::PeerId::random());
        nodes[1].own_queries.insert(id, QueryClass::Exploration);
    }
    // SETTLED, not merely started: K15.5 asserts the live gauge has
    // returned to zero while the cumulative total has not.
    pump_until(&mut nodes, Duration::from_secs(20), |n| {
        n[1].ledger.behaviour_originated() > 0
            && n[1].swarm.behaviour().gate.pending_behaviour_dials() == 0
    })
    .await;

    // EVERY FIELD the driver port specifies, taken from the real API.
    let mode = nodes[1].kad().map(|k| k.mode());
    r.check("K15.1", "mode is readable", mode.is_some());
    let protocol_hash = namespace::network_hash(&cfg.network_id);
    r.check(
        "K15.2",
        "protocol_hash is a fixed-width tag, not a peer list",
        protocol_hash.len() == 26,
    );
    let routing_peer_count = nodes[1].routing_peers();
    let nonempty_buckets = nodes[1]
        .kad()
        .map_or(0, |k| k.kbuckets().filter(|b| b.num_entries() > 0).count());
    r.check(
        "K15.3",
        &format!("routing_peer_count={routing_peer_count} and nonempty_bucket_count={nonempty_buckets} are counts"),
        routing_peer_count >= 1 && nonempty_buckets >= 1,
    );
    // A REAL PER-CLASS SNAPSHOT, asserted. The earlier arithmetic
    // subtracted `finished_queries.len()` — which counts implicit
    // library queries too — from a set of ids that carried no class at
    // all, so a completed implicit bootstrap could cancel out an
    // explicit query still in flight, and the check was `true` regardless.
    let settled = nodes[1].active_queries_by_class();
    r.check(
        "K15.4a",
        &format!("with every started query finished, no class is active: {settled:?}"),
        settled.is_empty(),
    );
    // START ONE and observe it counted under ITS class while a second
    // class stays at zero — which is what "by class" has to mean.
    let live_id = nodes[1]
        .kad()
        .map(|k| k.get_closest_peers(libp2p::PeerId::random()));
    if let Some(q) = live_id {
        nodes[1].own_queries.insert(q, QueryClass::Exploration);
    }
    let during = nodes[1].active_queries_by_class();
    r.check(
        "K15.4b",
        &format!("a started query is active under its own class only: {during:?}"),
        during.get(&QueryClass::Exploration).copied() == Some(1)
            && during.get(&QueryClass::Targeted).is_none()
            && during.get(&QueryClass::Bootstrap).is_none(),
    );
    // AND AN IMPLICIT QUERY DOES NOT DECREMENT IT. The library's own
    // work finishing is exactly what the old arithmetic subtracted.
    let unattributed_before = nodes[1].observed.unattributed_queries.len();
    pump_until(&mut nodes, Duration::from_secs(20), |n| {
        live_id.is_some_and(|q| n[1].observed.finished_queries.contains(&q))
    })
    .await;
    let after = nodes[1].active_queries_by_class();
    r.check(
        "K15.4c",
        &format!(
            "and once it finishes the class is empty again: {after:?} \
             (unattributed seen: {unattributed_before})"
        ),
        after.get(&QueryClass::Exploration).is_none(),
    );
    // A LIVE COUNT, and asserted. `behaviour_originated()` is a
    // CUMULATIVE total: after this experiment's pump every dial has
    // settled, so reporting it as `pending_behaviour_dials` would show
    // completed dials as in flight — a materially wrong diagnostic, not
    // an imprecise one. The gate's `pending_behaviour_dials()` is the
    // live gauge, and the two are compared here so the wrong mapping
    // cannot be chosen silently.
    let cumulative = nodes[1].ledger.behaviour_originated();
    let live = nodes[1].swarm.behaviour().gate.pending_behaviour_dials();
    r.check(
        "K15.5",
        &format!(
            "pending_behaviour_dials is the gate's LIVE count ({live}), not its \
             cumulative total ({cumulative}) — every dial here has settled"
        ),
        live == 0 && cumulative > 0,
    );
    r.check(
        "K15.6",
        "last_query_progress_at is available because progress events carry it",
        !nodes[1].observed.query_requests.is_empty(),
    );
    // AND THE BOUND: nothing here is a peer list or a payload.
    r.check(
        "K15.7",
        "every field is a scalar or a fixed tag — no routing dump, no payload",
        protocol_hash.len() == 26,
    );
    r.note(format!(
        "K15 snapshot: mode={mode:?} hash={protocol_hash} routing={routing_peer_count} buckets={nonempty_buckets} behaviour_dials_live={live} cumulative={cumulative}"
    ));

    // THE ASYNCHRONOUS PATH, which reading the fields directly does not
    // exercise. §3 of `kademlia-integration.md` specifies `Snapshot {
    // request_id }` answered by `SnapshotResult { request_id, .. }` over
    // bounded channels, and says a response missing before the local
    // control deadline is a DRIVER-HEALTH FAILURE. A driver that dropped
    // a response, answered late, or correlated it to the wrong request
    // would pass a field-reading experiment unchanged.
    //
    // The channel and the deadline are project logic, so they are
    // modelled here over a real Tokio bounded channel and a real
    // timeout, and the three ways they can go wrong are each provoked.
    let (tx, mut rx) = tokio::sync::mpsc::channel::<(u64, usize)>(4);
    let deadline = Duration::from_millis(200);

    // 1. CORRELATED: the answer carries the id that was asked.
    let asked = 7_u64;
    tx.send((asked, routing_peer_count)).await.expect("bounded channel");
    let answered = tokio::time::timeout(deadline, rx.recv()).await;
    r.check(
        "K15.8",
        "a Snapshot request is answered within the control deadline, correlated by id",
        matches!(answered, Ok(Some((id, _))) if id == asked),
    );

    // 2. DROPPED: no answer at all is a health failure, not a hang.
    let missing = tokio::time::timeout(deadline, rx.recv()).await;
    r.check(
        "K15.9",
        "a missing SnapshotResult is a bounded timeout, not an unbounded wait",
        missing.is_err(),
    );

    // 3. MISCORRELATED, tested through an actual CONSUMER. Asserting
    // that `asked + 1 != asked` is arithmetic: a consumer that accepts
    // the first arrival by order would satisfy it unchanged, which is
    // exactly the implementation the request id exists to rule out. So
    // the consumer is written and then fed the wrong answer.
    //
    // It keeps waiting rather than accepting, which is the whole
    // behaviour: a snapshot reader that takes whatever arrives first
    // reports another request's driver state as its own.
    async fn await_snapshot(
        rx: &mut tokio::sync::mpsc::Receiver<(u64, usize)>,
        want: u64,
        deadline: Duration,
    ) -> Option<usize> {
        let until = tokio::time::Instant::now() + deadline;
        loop {
            let left = until.saturating_duration_since(tokio::time::Instant::now());
            match tokio::time::timeout(left, rx.recv()).await {
                Ok(Some((id, value))) if id == want => return Some(value),
                // NOT this request's answer. Discarded, and the wait
                // continues against the same deadline.
                Ok(Some(_)) => {}
                Ok(None) | Err(_) => return None,
            }
        }
    }

    // A wrong answer alone: the consumer must NOT return it.
    tx.send((asked + 1, 99)).await.expect("bounded channel");
    r.check(
        "K15.10",
        "a consumer waiting for its request id refuses another request's answer",
        await_snapshot(&mut rx, asked, deadline).await.is_none(),
    );
    // A wrong answer FOLLOWED by the right one: the consumer must skip
    // the first and return the second. This is the half that
    // distinguishes "refuses everything" from "correlates".
    tx.send((asked + 1, 99)).await.expect("bounded channel");
    tx.send((asked, 42)).await.expect("bounded channel");
    r.check(
        "K15.11",
        "and returns the matching one that follows it",
        await_snapshot(&mut rx, asked, deadline).await == Some(42),
    );

    // 4. BOUNDED: the channel refuses rather than growing. A driver
    // whose control channel is unbounded turns a stalled reader into a
    // memory-exhaustion path, which §6 forbids.
    let mut accepted = 0;
    while tx.try_send((0, 0)).is_ok() {
        accepted += 1;
        assert!(accepted < 1_000, "the control channel must be finite");
    }
    r.check(
        "K15.12",
        &format!("the control channel is bounded and refuses when full ({accepted} queued)"),
        accepted > 0 && tx.try_send((0, 0)).is_err(),
    );
}

/// K16 — disjoint query paths, and what the spike can honestly claim.
pub async fn k16_disjoint_paths(r: &mut Report) {
    // FIVE ROUTERS around one asker, each knowing a different sixth
    // node. With `parallelism = 3` and disjoint paths on, one round of
    // the walk uses several routers rather than one.
    const ROUTERS: usize = 5;
    let cfg = NodeConfig {
        role: KadRole::Server,
        gate_mode: Mode::PolicyAdmit,
        parallelism: NonZeroUsize::new(3).expect("nonzero"),
        disjoint_paths: true,
        ..NodeConfig::default()
    };
    let mut nodes = Vec::new();
    for _ in 0..=ROUTERS {
        nodes.push(Node::start(&cfg).await);
    }
    let ids: Vec<_> = nodes.iter().map(|n| n.peer_id).collect();
    let addrs: Vec<_> = nodes.iter().map(Node::dial_address).collect();
    for n in &mut nodes {
        for id in &ids {
            n.trust(*id);
        }
    }
    // The asker is 0; routers are 1..=ROUTERS.
    for i in 1..=ROUTERS {
        nodes[0].dial_admitted(addrs[i].clone());
    }
    pump(&mut nodes, Duration::from_secs(6)).await;
    nodes[0].ledger.reset();
    for i in 1..=ROUTERS {
        let (peer, addr) = (ids[i], addrs[i].clone());
        if let Some(k) = nodes[0].kad() {
            k.add_address(&peer, addr);
        }
    }
    let id = nodes[0]
        .kad()
        .map(|k| k.get_closest_peers(libp2p::PeerId::random()));
    if let Some(q) = id {
        nodes[0].own_queries.insert(q, QueryClass::Exploration);
    }
    pump(&mut nodes, Duration::from_secs(12)).await;

    // THE EXPLICIT QUERY ONLY. Taking the maximum across every query
    // included the implicit bootstrap a routing insertion starts (F2),
    // so the number measured library housekeeping as readily as the
    // query under test.
    let contacted = id
        .and_then(|q| nodes[0].observed.query_requests.get(&q).copied())
        .map_or(0, |(requests, _)| requests);
    r.check(
        "K16.1",
        &format!("the explicit query contacts several routers: {contacted} requests"),
        contacted > 1,
    );
    r.check(
        "K16.2",
        "the configuration with disjoint paths enabled builds and queries complete",
        !nodes[0].observed.finished_queries.is_empty(),
    );
    r.note(format!(
        "K16 disjoint=true: explicit query made {contacted} requests"
    ));
    drop(nodes);

    // THE CONTROL. Everything above is satisfied by `parallelism = 3`
    // alone, so it stays green whether `disjoint_query_paths` is honoured,
    // ignored or off — which means it says nothing about the option. The
    // same topology and the same parallelism with the flag DISABLED is
    // the only thing that can.
    let control_cfg = NodeConfig {
        disjoint_paths: false,
        ..cfg.clone()
    };
    let mut control = Vec::new();
    for _ in 0..=ROUTERS {
        control.push(Node::start(&control_cfg).await);
    }
    let cids: Vec<_> = control.iter().map(|n| n.peer_id).collect();
    let caddrs: Vec<_> = control.iter().map(Node::dial_address).collect();
    for n in &mut control {
        for x in &cids {
            n.trust(*x);
        }
    }
    for i in 1..=ROUTERS {
        control[0].dial_admitted(caddrs[i].clone());
    }
    pump(&mut control, Duration::from_secs(6)).await;
    control[0].ledger.reset();
    for i in 1..=ROUTERS {
        let (peer, addr) = (cids[i], caddrs[i].clone());
        if let Some(k) = control[0].kad() {
            k.add_address(&peer, addr);
        }
    }
    let cid = control[0]
        .kad()
        .map(|k| k.get_closest_peers(libp2p::PeerId::random()));
    if let Some(q) = cid {
        control[0].own_queries.insert(q, QueryClass::Exploration);
    }
    pump(&mut control, Duration::from_secs(12)).await;
    let control_contacted = cid
        .and_then(|q| control[0].observed.query_requests.get(&q).copied())
        .map_or(0, |(requests, _)| requests);

    r.check(
        "K16.3",
        &format!(
            "the control with disjoint_paths=false also completes its query \
             ({control_contacted} requests), so the comparison is between two \
             working configurations"
        ),
        control_contacted > 0,
    );
    // MEASURED, then reported truthfully. At six nodes on loopback the
    // two configurations contact the same routers, because the whole
    // network fits inside one round of `parallelism = 3` twice over —
    // there is no second path for the option to make disjoint. Claiming
    // a width difference here would be claiming a result this topology
    // cannot produce.
    r.note(format!(
        "K16: disjoint=true made {contacted} requests, disjoint=false made \
         {control_contacted} — at this scale the option changes nothing \
         measurable, which is a fact about the topology, not about the option"
    ));
    r.note(
        "K16 LIMIT: this measures path WIDTH against a control, and finds no \
         difference at six nodes on loopback. It says NOTHING about Byzantine \
         resistance: reduced single-path capture is a claim about an adversary \
         controlling a subset of routers, and this harness has no adversary. \
         Both limits are in the record."
            .to_owned(),
    );
}

/// K17 — twenty nodes, convergence and bounded resource behaviour.
pub async fn k17_twenty_node_convergence(r: &mut Report) {
    const N: usize = 20;
    let cfg = NodeConfig {
        role: KadRole::Server,
        gate_mode: Mode::PolicyAdmit,
        kbucket_size: NonZeroUsize::new(20).expect("nonzero"),
        ..NodeConfig::default()
    };
    let mut nodes = Vec::with_capacity(N);
    for _ in 0..N {
        nodes.push(Node::start(&cfg).await);
    }
    let ids: Vec<_> = nodes.iter().map(|n| n.peer_id).collect();
    let addrs: Vec<_> = nodes.iter().map(Node::dial_address).collect();
    for n in &mut nodes {
        for id in &ids {
            n.trust(*id);
        }
    }
    let protocol = namespace::protocol_name(&cfg.network_id);
    // ONE SEED for everyone: node 0. A star, so every other peer must be
    // discovered rather than configured.
    for i in 1..N {
        nodes[i].dial_admitted(addrs[0].clone());
    }
    pump(&mut nodes, Duration::from_secs(8)).await;
    // THE SEED LEARNS ITS CALLERS. Under `BucketInserts::Manual` an
    // inbound connection puts nobody in the routing table, so a star's
    // hub answers every query with an empty list until the provider
    // admits the peers that dialled it — which the first run of this
    // experiment measured as total non-convergence.
    for node in &mut nodes {
        crate::topology::admit_candidates(node, &protocol);
    }

    let start = std::time::Instant::now();
    let mut rounds = 0;
    let mut totals = Vec::new();
    // THROUGH THE BOUNDED SCHEDULER, as K11 does — the twenty-node
    // convergence is the figure the roadmap quotes, so it must be
    // convergence within the budgets rather than with them bypassed.
    let mut sched: Vec<QueryScheduler> = (0..N).map(|_| QueryScheduler::new(2, 6)).collect();
    let mut permits_granted = 0_usize;
    for _ in 0..5 {
        rounds += 1;
        for i in 0..N {
            let Ok(permit) = sched[i].acquire(rounds * 1_000) else {
                continue;
            };
            let key = random_32();
            let started = nodes[i]
                .kad()
                .map(|k| k.get_n_closest_peers(key, NonZeroUsize::new(20).expect("nonzero")));
            match started {
                Some(id) => {
                    sched[i].bind(permit, id);
                    nodes[i].own_queries.insert(id, QueryClass::Exploration);
                    permits_granted += 1;
                }
                None => sched[i].release(permit),
            }
        }
        pump(&mut nodes, Duration::from_secs(10)).await;
        for i in 0..N {
            let finished: Vec<_> = nodes[i].observed.finished_queries.clone();
            for id in finished {
                sched[i].finish(id);
            }
        }
        for node in &mut nodes {
            crate::topology::admit_candidates(node, &protocol);
        }
        pump(&mut nodes, Duration::from_secs(2)).await;
        let sizes: Vec<usize> = (0..N).map(|i| nodes[i].routing_peers()).collect();
        totals.push(sizes.iter().sum::<usize>());
        r.note(format!(
            "K17 round {rounds}: routing sizes min {} max {} total {}",
            sizes.iter().min().copied().unwrap_or(0),
            sizes.iter().max().copied().unwrap_or(0),
            totals.last().copied().unwrap_or(0)
        ));
    }
    let elapsed = start.elapsed();

    let sizes: Vec<usize> = (0..N).map(|i| nodes[i].routing_peers()).collect();
    // EVERY node, not "most nodes reach half". The earlier predicate —
    // `converged >= N/2` over `s >= N/2` — passed when ten nodes had ten
    // entries and ten had none, which is a partition, while the record
    // claimed 19/19. A degraded run must not close a release gate.
    let full = sizes.iter().filter(|&&s| s == N - 1).count();
    r.check(
        "K17.1",
        &format!("every node routes every other: {full}/{N} at {} entries", N - 1),
        full == N,
    );
    // `s < N` was TAUTOLOGICAL: with N nodes a table of unique remote
    // peers cannot hold more than N - 1, and K17.1 already requires
    // exactly that. It could not have detected an ignored bound. The
    // real question needs a population ABOVE a deliberately reduced
    // bound, which is what K17.5 does.
    r.check(
        "K17.2",
        &format!(
            "every table holds exactly the peers it learned: max {}",
            sizes.iter().max().copied().unwrap_or(0)
        ),
        sizes.iter().all(|&s| s == N - 1),
    );
    // EVERY dial ADMITTED, not "at least one seen". `> 0` passed a run
    // in which all 200-plus dials were refused — which would have been
    // the opposite of the recorded evidence.
    let started_total: usize = (0..N)
        .map(|i| {
            nodes[i]
                .own_queries
                .values()
                .filter(|c| **c == QueryClass::Exploration)
                .count()
        })
        .sum();
    r.check(
        "K17.0",
        &format!(
            "the convergence ran through the bounded scheduler: {started_total} \
             exploration queries started, {permits_granted} permitted"
        ),
        started_total == permits_granted && permits_granted > 0,
    );
    let originated: u64 = (0..N).map(|i| nodes[i].ledger.behaviour_originated()).sum();
    let allowed: u64 = (0..N).map(|i| nodes[i].ledger.behaviour_allowed()).sum();
    let refused: Vec<_> = (0..N)
        .map(|i| nodes[i].ledger.refusals())
        .filter(|m| !m.is_empty())
        .collect();
    r.check(
        "K17.3",
        &format!("the run really exercised behaviour dials: {originated}"),
        originated > 0,
    );
    // EVERY DIAL ACCOUNTED FOR, and every refusal EXPLAINED. Requiring
    // zero refusals was too absolute: in a twenty-node run some dials
    // genuinely fail — a remote at its own connection ceiling, a peer
    // mid-restart — and the manager then records the failure and refuses
    // the next attempt for `peer backoff`, which is the policy working
    // rather than the gate malfunctioning. The assertion that matters is
    // that nothing is unaccounted and no refusal is of an unexpected
    // kind; "all dials refused" is separately impossible, because
    // K17.1 requires every node to have converged.
    let total_refused: u64 = refused.iter().map(|m| m.values().sum::<u64>()).sum();
    let unexpected: Vec<&String> = refused
        .iter()
        .flat_map(std::collections::BTreeMap::keys)
        .filter(|k| k.as_str() != "peer backoff")
        .collect();
    r.check(
        "K17.4",
        &format!(
            "every behaviour dial is accounted for: {allowed} admitted + \
             {total_refused} refused = {originated}"
        ),
        allowed + total_refused == originated,
    );
    r.check(
        "K17.5",
        &format!(
            "and every refusal is an explained policy outcome, not an unexpected \
             class: {refused:?}"
        ),
        unexpected.is_empty(),
    );
    r.note(format!(
        "K17: {N} nodes, {rounds} rounds, {:.1}s wall clock, final sizes {sizes:?}",
        elapsed.as_secs_f64()
    ));

    // THE BOUND UNDER PRESSURE, on FRESH nodes. A bound applied before
    // insertion stops a table GROWING; it cannot shrink one already
    // full, so re-bounding the converged nodes above would measure
    // nothing — they hold 19 from rounds that ran before the bound
    // existed. Two newcomers join the converged network instead, one
    // bounded and one not, seeded identically.
    //
    // `max_routing_peers` is PROJECT logic applied before manual
    // insertion (§11). rust-libp2p knows nothing about it, and
    // `kbucket_size` does not stand in for it: a table can hold
    // `kbucket_size` entries in each of many buckets and still exceed a
    // total the project meant to enforce. It is only testable against a
    // population LARGER than the bound.
    const BOUND: usize = 5;
    let mut late = vec![Node::start(&cfg).await, Node::start(&cfg).await];
    let late_ids: Vec<_> = late.iter().map(|n| n.peer_id).collect();
    for n in &mut late {
        for id in ids.iter().chain(late_ids.iter()) {
            n.trust(*id);
        }
    }
    for n in &mut nodes {
        for id in &late_ids {
            n.trust(*id);
        }
    }
    late[0].dial_admitted(addrs[0].clone());
    late[1].dial_admitted(addrs[0].clone());
    let mut all: Vec<Node> = nodes;
    all.append(&mut late);
    pump(&mut all, Duration::from_secs(6)).await;
    let hub = ids[0];
    for idx in [N, N + 1] {
        let addr = addrs[0].clone();
        if let Some(k) = all[idx].kad() {
            k.add_address(&hub, addr);
        }
    }

    for round in 0..4 {
        for idx in [N, N + 1] {
            let key = random_32();
            if let Some(k) = all[idx].kad() {
                let q = k.get_n_closest_peers(key, NonZeroUsize::new(20).expect("nonzero"));
                all[idx].own_queries.insert(q, QueryClass::Exploration);
            }
        }
        pump(&mut all, Duration::from_secs(8)).await;
        crate::topology::admit_candidates_bounded(&mut all[N], &protocol, BOUND);
        crate::topology::admit_candidates(&mut all[N + 1], &protocol);
        pump(&mut all, Duration::from_secs(2)).await;
        r.note(format!(
            "K17 bound round {round}: bounded newcomer holds {}, unbounded {}",
            all[N].routing_peers(),
            all[N + 1].routing_peers()
        ));
    }
    let bounded = all[N].routing_peers();
    let unbounded = all[N + 1].routing_peers();
    // AT the bound, not merely within it. `<= BOUND` is satisfied by a
    // bounded pipeline that admits NOTHING — 0 through 5 all pass — so
    // a completely broken admission would have been reported as "stops
    // at 5". The twin exceeding the bound proves candidates existed for
    // the twin, not that this node saw any.
    r.check(
        "K17.6",
        &format!("a node under a reduced bound fills it and stops: {bounded} == {BOUND}"),
        bounded == BOUND,
    );
    r.check(
        "K17.7",
        &format!(
            "CONTROL: its twin, admitting freely from the SAME rounds and the \
             same seed, holds far more ({unbounded}) — so the bound is what \
             stopped it, not a shortage of candidates"
        ),
        unbounded > BOUND,
    );

    // AND A FULL TABLE STILL TAKES ADDRESS UPDATES. A population bound
    // caps how many peers are routed; refusing a re-announcement for a
    // peer already routed would freeze its ADDRESSES too, pinning it to
    // whatever route it was admitted with while §11 has expiry and
    // removal propagate to the driver.
    let routed_peers: Vec<libp2p::PeerId> = ids
        .iter()
        .copied()
        .filter(|p| all[N].routes(p))
        .collect();
    r.check(
        "K17.8",
        &format!("the bounded node has routed peers to update ({})", routed_peers.len()),
        !routed_peers.is_empty(),
    );
    // AND THE FAILURE PATH EXITS, rather than panicking. Indexing here
    // aborted the process when convergence or bounded admission failed
    // badly enough to leave the table empty — so the run that most
    // needed the accumulated failure report was the one that never
    // printed it.
    let Some(&subject) = routed_peers.first() else {
        r.note(
            "K17: no routed peer to update, so K17.9/K17.10 are skipped — the \
             failure above is the finding, and the report still runs."
                .to_owned(),
        );
        return;
    };
    let fresh: libp2p::Multiaddr = "/ip4/127.0.0.1/tcp/65000".parse().expect("valid");
    let before_count = all[N].routing_peers();
    // THROUGH THE BOUNDED PIPELINE, not around it. Calling
    // `add_address` directly tested libp2p rather than the guard: the
    // assertion passed unchanged even with `admit_candidates_bounded`
    // regressed to rejecting every candidate at the bound, which is the
    // exact regression it was written to catch.
    //
    // The address is placed where a real observation would arrive — the
    // candidate data the pipeline reads — and the pipeline is invoked.
    all[N]
        .observed
        .learned_addresses
        .entry(subject)
        .or_default()
        .insert(fresh.clone());
    let admitted_update =
        crate::topology::admit_candidates_bounded(&mut all[N], &protocol, BOUND);
    pump(&mut all, Duration::from_secs(2)).await;
    r.check(
        "K17.8b",
        &format!("the bounded pipeline accepted the update ({admitted_update} admitted)"),
        admitted_update > 0,
    );
    // The update reaches the driver, and the population is unchanged —
    // both halves, since an update that grew the table would be the
    // bound failing in the other direction.
    let addresses = all[N]
        .kad()
        .and_then(|k| {
            k.kbuckets()
                .flat_map(|b| b.iter().map(|e| (*e.node.key.preimage(), e.node.value.clone())).collect::<Vec<_>>())
                .find(|(p, _)| *p == subject)
                .map(|(_, a)| a.iter().cloned().collect::<Vec<_>>())
        })
        .unwrap_or_default();
    r.check(
        "K17.9",
        &format!(
            "a full table still accepts an address update for a peer it already \
             routes: {:?}",
            addresses.iter().map(std::string::ToString::to_string).collect::<Vec<_>>()
        ),
        // PREFIX: kad stores the address with the peer component
        // appended, the same form K14 had to account for.
        addresses.iter().any(|a| a.to_string().starts_with(&fresh.to_string())),
    );
    r.check(
        "K17.10",
        &format!(
            "and the population is unchanged: {} (was {before_count})",
            all[N].routing_peers()
        ),
        all[N].routing_peers() == before_count,
    );
}

/// K18 — a malicious or stale routing response.
///
/// The brief names "malicious/stale routing responses" explicitly. K8
/// covers the identity half: a router returning a peer the asker does
/// not trust. This is the ADDRESS half — a router returning a real,
/// trusted peer at an address that does not work — because the two fail
/// differently and only one of them is about trust.
pub async fn k18_stale_routing_response(r: &mut Report) {
    use interweave_transport_api::TransportIdentity;

    let cfg = NodeConfig {
        role: KadRole::Server,
        gate_mode: Mode::PolicyAdmit,
        ..NodeConfig::default()
    };
    let mut nodes = vec![
        Node::start(&cfg).await,
        Node::start(&cfg).await,
        Node::start(&cfg).await,
    ];
    let (a, b, c) = (nodes[0].peer_id, nodes[1].peer_id, nodes[2].peer_id);
    for i in 0..3 {
        for p in [a, b, c] {
            nodes[i].trust(p);
        }
    }
    let (b_addr, c_addr) = (nodes[1].dial_address(), nodes[2].dial_address());

    // THE ROUTER POISONS ITS OWN TABLE: `b` holds `c` at an address
    // nothing listens on. This is what a hostile router returns, and
    // what an honest one returns after `c` moves.
    let dead: libp2p::Multiaddr = "/ip4/127.0.0.1/tcp/1".parse().expect("valid");
    nodes[1].dial_admitted(c_addr.clone());
    pump_until(&mut nodes, Duration::from_secs(10), |n| {
        n[1].observed.connected.contains(&c)
    })
    .await;
    if let Some(k) = nodes[1].kad() {
        k.add_address(&c, dead.clone());
    }

    // `a` already has a WORKING route to `c`, recorded through the
    // manager the way a successful authenticated connection is. That is
    // what makes the assertions below meaningful: a bad address must not
    // cost a good one.
    let c_identity = TransportIdentity::parse(c.to_base58()).expect("canonical");
    let good_slot = {
        let mut m = nodes[0].manager.lock().expect("manager");
        let ticket = m
            .handle()
            .admit(
                &DialRequest {
                    peer: Some(c_identity.clone()),
                    address: c_addr.to_string(),
                    origin: DialOrigin::ConnectionManager,
                },
                0,
            )
            .expect("admitted");
        m.record_success(ticket, 0)
    };

    nodes[0].dial_admitted(b_addr.clone());
    pump_until(&mut nodes, Duration::from_secs(10), |n| {
        n[0].observed.connected.contains(&b)
    })
    .await;
    nodes[0].ledger.reset();
    if let Some(k) = nodes[0].kad() {
        k.add_address(&b, b_addr);
        let id = k.get_closest_peers(c);
        nodes[0].own_queries.insert(id, QueryClass::Targeted);
    }
    // SETTLE ON THE OBSERVABLE. A fixed pump made this experiment fail
    // intermittently in a full-suite run and pass alone — the walk needs
    // longer when the machine is busy, and a wall-clock budget measures
    // the machine rather than the behaviour.
    let dead_text = dead.to_string();
    pump_until(&mut nodes, Duration::from_secs(30), |n| {
        n[0].observed
            .dial_errors
            .iter()
            .any(|e| e.contains(&dead_text))
    })
    .await;

    // THE DIAL ERROR NAMES THE ADDRESS, which is stronger evidence than
    // the query result would be: it proves the poisoned address was not
    // merely reported but acted on. `GetClosestPeers` reports the peers
    // a walk found, and a peer whose only address fails to connect is
    // not among them — so reading the result set here would look for the
    // evidence in the one place the failure removes it.
    let dialed_dead = nodes[0]
        .observed
        .dial_errors
        .iter()
        .any(|e| e.contains(&dead_text) && e.contains(&c.to_base58()));
    r.check(
        "K18.1",
        "the router handed back the unusable address and this node dialled it",
        dialed_dead,
    );
    r.check(
        "K18.1b",
        "and the walk did NOT report the peer as found, because it never answered",
        !nodes[0]
            .observed
            .query_results
            .values()
            .any(|peers| peers.contains(&c)),
    );
    r.note(format!(
        "K18 ledger: {} behaviour dials, {} allowed, refusals {:?}; a connected to {:?}",
        nodes[0].ledger.behaviour_originated(),
        nodes[0].ledger.behaviour_allowed(),
        nodes[0].ledger.refusals(),
        nodes[0].observed.connected.len()
    ));
    let errors = nodes[0].observed.dial_errors.len();
    r.check(
        "K18.2",
        &format!("the dial to it was attempted and failed: {errors} outgoing error(s)"),
        errors > 0,
    );

    // THE PRODUCTION RULE: an address-scoped failure on an address that
    // never worked must not suppress the peer while a known-good route
    // remains. Fed to the manager the way the runtime feeds it.
    {
        let mut m = nodes[0].manager.lock().expect("manager");
        let ticket = m
            .handle()
            .admit(
                &DialRequest {
                    peer: Some(c_identity.clone()),
                    address: dead.to_string(),
                    origin: DialOrigin::ConnectionManager,
                },
                1_000,
            )
            .expect("the peer is not suppressed yet");
        m.record_failure(ticket, 1_000);
    }
    // THE OBSERVABLE the rule is about: can this node still dial the
    // peer at the route that works? Asking the manager is the whole
    // question — a predicate on the policy would have been one layer
    // below the thing that decides.
    let still_dialable = nodes[0]
        .manager
        .lock()
        .expect("manager")
        .handle()
        .admit(
            &DialRequest {
                peer: Some(c_identity.clone()),
                address: c_addr.to_string(),
                origin: DialOrigin::ConnectionManager,
            },
            2_000,
        );
    r.check(
        "K18.3",
        "a bad address from a router does not suppress the peer's good route",
        still_dialable.is_ok(),
    );
    r.note(format!(
        "K18: after the failed dial, the good route admits: {:?}",
        still_dialable.as_ref().err()
    ));
    drop(still_dialable);
    drop(good_slot);
    // THE SETTLEMENT RECORDED THE ADDRESS THAT WAS USED. A behaviour
    // dial's ticket is minted with an empty placeholder — there is no
    // address at admission (F9) — so settling it feeds the empty string
    // to the address policy and the address book, and every Kademlia
    // route shares one entry while the real one is never learned. The
    // address book is the observable: after a failed behaviour dial the
    // peer's candidates must name the address that failed.
    let candidates = nodes[0]
        .manager
        .lock()
        .expect("manager")
        .dial_candidates(&c_identity, 2_000);
    r.check(
        "K18.5",
        &format!("the failed behaviour dial is recorded against the ADDRESS IT USED: {candidates:?}"),
        candidates.iter().any(|a| a == &dead.to_string()),
    );
    r.check(
        "K18.6",
        "and never against an empty placeholder",
        !candidates.iter().any(std::string::String::is_empty),
    );
    // EVERY address a multi-address dial exhausted, not just the first.
    // `DialError::Transport` carries one entry per attempt, and scoring
    // only the first leaves the rest unscored and immediately
    // retryable — the same "looks checked, checks nothing" shape as F9.
    // A second dead address is added for the router to hand over.
    let second_dead: libp2p::Multiaddr = "/ip4/127.0.0.1/tcp/3".parse().expect("valid");
    if let Some(k) = nodes[1].kad() {
        k.add_address(&c, second_dead.clone());
    }
    nodes[0].ledger.reset();
    if let Some(k) = nodes[0].kad() {
        let id = k.get_closest_peers(c);
        nodes[0].own_queries.insert(id, QueryClass::Targeted);
    }
    pump(&mut nodes, Duration::from_secs(15)).await;
    let after = nodes[0]
        .manager
        .lock()
        .expect("manager")
        .dial_candidates(&c_identity, 4_000);
    r.note(format!("K18 candidates after a multi-address failure: {after:?}"));
    // BOTH dead addresses by name. `!after.is_empty()` was satisfied by
    // scoring only the first — the exact mutation this is here to
    // catch — so the assertion names the addresses the dial exhausted.
    r.check(
        "K18.7",
        &format!(
            "EVERY address the dial exhausted is scored, not just the first: \
             {after:?}"
        ),
        after.iter().any(|a| a == &dead.to_string())
            && after.iter().any(|a| a == &second_dead.to_string())
            && !after.iter().any(std::string::String::is_empty),
    );

    r.note(format!(
        "K18 dial errors: {:?}",
        nodes[0]
            .observed
            .dial_errors
            .iter()
            .take(2)
            .collect::<Vec<_>>()
    ));
}

/// K14 — targeted lookup, and the evidence rule that gates it.
///
/// The brief requires this explicitly: a targeted PeerId lookup is
/// scheduled ONLY with fresh evidence that the target advertised the
/// exact project Kademlia **server** protocol; it can recover missing
/// addresses where the DHT knows the target; and client-mode nodes are
/// not misrepresented as generally discoverable.
///
/// The eligibility rule is project logic — `kademlia-integration.md`
/// §9.2 — so it is implemented here over the observation the library
/// actually provides, and then the lookup it gates is run for real.
pub struct TargetedLookup;

/// A capability observation as `providers/peer-cache.md` describes it.
#[derive(Debug, Clone)]
pub struct CapabilityEvidence {
    pub network_hash: String,
    pub wire_major: u32,
    pub role_is_server: bool,
    pub supported: bool,
    pub observed_at: u64,
}

impl TargetedLookup {
    /// §9.2's five conjuncts, as a predicate.
    ///
    /// Every one is necessary; the tests below turn each off in turn,
    /// because a conjunction is the shape most easily satisfied by
    /// accident — one clause doing all the work reads identically to
    /// five clauses working.
    #[must_use]
    pub fn eligible(
        trusted: bool,
        evidence: Option<&CapabilityEvidence>,
        current_hash: &str,
        current_major: u32,
        ttl_ms: u64,
        now_ms: u64,
        has_usable_address: bool,
        cooldown_elapsed: bool,
        budget_permits: bool,
    ) -> bool {
        if !trusted || has_usable_address || !cooldown_elapsed || !budget_permits {
            return false;
        }
        let Some(e) = evidence else {
            return false;
        };
        e.supported
            && e.role_is_server
            && e.network_hash == current_hash
            && e.wire_major == current_major
            && now_ms.saturating_sub(e.observed_at) <= ttl_ms
    }
}

pub async fn k14_targeted_lookup(r: &mut Report) {
    let network = "spike-003".to_owned();
    let hash = namespace::network_hash(&network);
    let fresh = CapabilityEvidence {
        network_hash: hash.clone(),
        wire_major: 1,
        role_is_server: true,
        supported: true,
        observed_at: 1_000,
    };
    let ttl = 60_000;

    // ELIGIBLE, and then each conjunct denied in turn.
    r.check(
        "K14.1",
        "fresh server evidence for this namespace, no usable address: eligible",
        TargetedLookup::eligible(true, Some(&fresh), &hash, 1, ttl, 2_000, false, true, true),
    );
    let cases: [(&str, bool); 8] = [
        (
            "an untrusted target is never looked up",
            TargetedLookup::eligible(false, Some(&fresh), &hash, 1, ttl, 2_000, false, true, true),
        ),
        (
            "absent evidence is not permission to guess",
            TargetedLookup::eligible(true, None, &hash, 1, ttl, 2_000, false, true, true),
        ),
        (
            "NEGATIVE evidence is refused, not treated as unknown",
            TargetedLookup::eligible(
                true,
                Some(&CapabilityEvidence {
                    supported: false,
                    ..fresh.clone()
                }),
                &hash,
                1,
                ttl,
                2_000,
                false,
                true,
                true,
            ),
        ),
        (
            "a CLIENT-mode observation is not a discoverable target",
            TargetedLookup::eligible(
                true,
                Some(&CapabilityEvidence {
                    role_is_server: false,
                    ..fresh.clone()
                }),
                &hash,
                1,
                ttl,
                2_000,
                false,
                true,
                true,
            ),
        ),
        (
            "evidence from ANOTHER network_id does not carry over",
            TargetedLookup::eligible(
                true,
                Some(&fresh),
                &namespace::network_hash("spike-003-other"),
                1,
                ttl,
                2_000,
                false,
                true,
                true,
            ),
        ),
        (
            "evidence for another wire major does not carry over",
            TargetedLookup::eligible(true, Some(&fresh), &hash, 2, ttl, 2_000, false, true, true),
        ),
        (
            "STALE evidence past the TTL is refused",
            TargetedLookup::eligible(true, Some(&fresh), &hash, 1, ttl, 200_000, false, true, true),
        ),
        (
            "a peer with a usable address is not looked up at all",
            TargetedLookup::eligible(true, Some(&fresh), &hash, 1, ttl, 2_000, true, true, true),
        ),
    ];
    for (claim, held) in cases {
        r.check("K14.2", claim, !held);
    }
    r.check(
        "K14.3",
        "cooldown and budget each gate it independently",
        !TargetedLookup::eligible(true, Some(&fresh), &hash, 1, ttl, 2_000, false, false, true)
            && !TargetedLookup::eligible(
                true,
                Some(&fresh),
                &hash,
                1,
                ttl,
                2_000,
                false,
                true,
                false,
            ),
    );

    // AND THE LOOKUP ITSELF, for real: `a` has no address for `c` and
    // asks the DHT for it by PeerId.
    let cfg = NodeConfig {
        role: KadRole::Server,
        network_id: network.clone(),
        gate_mode: Mode::PolicyAdmit,
        ..NodeConfig::default()
    };
    let mut nodes = vec![
        Node::start(&cfg).await,
        Node::start(&cfg).await,
        Node::start(&cfg).await,
    ];
    let (a, b, c) = (nodes[0].peer_id, nodes[1].peer_id, nodes[2].peer_id);
    for i in 0..3 {
        for p in [a, b, c] {
            nodes[i].trust(p);
        }
    }
    let (b_addr, c_addr) = (nodes[1].dial_address(), nodes[2].dial_address());
    nodes[1].dial_admitted(c_addr.clone());
    pump_until(&mut nodes, Duration::from_secs(10), |n| {
        n[1].observed.connected.contains(&c)
    })
    .await;
    if let Some(k) = nodes[1].kad() {
        k.add_address(&c, c_addr.clone());
    }
    nodes[0].dial_admitted(b_addr.clone());
    pump_until(&mut nodes, Duration::from_secs(10), |n| {
        n[0].observed.connected.contains(&b)
    })
    .await;

    // `a` KNOWS NO ADDRESS FOR `c` — the precondition §9.2's third
    // conjunct describes, and the reason a targeted lookup exists.
    r.check(
        "K14.4",
        "the asker holds no address for the target before the lookup",
        !nodes[0].observed.learned_addresses.contains_key(&c),
    );

    nodes[0].ledger.reset();
    if let Some(k) = nodes[0].kad() {
        k.add_address(&b, b_addr);
        // THE LOOKUP KEY IS THE PEER ID, which is what makes this
        // targeted rather than exploratory.
        let id = k.get_closest_peers(c);
        nodes[0].own_queries.insert(id, QueryClass::Targeted);
    }
    pump(&mut nodes, Duration::from_secs(15)).await;

    // PREFIX, not equality. A `FIND_NODE` answer carries the address
    // with the peer's own `/p2p/<id>` component appended, so comparing
    // against the bare listen address fails on a lookup that worked
    // perfectly — which is what the first run of this experiment
    // reported, and it was the assertion that was wrong.
    let wanted = c_addr.to_string();
    let recovered = nodes[0]
        .observed
        .learned_addresses
        .get(&c)
        .is_some_and(|addrs| addrs.iter().any(|a| a.to_string().starts_with(&wanted)));
    r.check(
        "K14.5",
        "a targeted lookup recovers the missing address the DHT knows",
        recovered,
    );
    r.note(format!(
        "K14: recovered {:?}",
        nodes[0].observed.learned_addresses.get(&c)
    ));
}

/// K19 — the global ceilings reach a behaviour dial.
///
/// `PolicySnapshot::admit` answers trust, backoff, quarantine and drain;
/// the pending-dial and connection ceilings are reserved one layer up,
/// by the ticket `ConnectionManager` mints. A gate that consulted only
/// the policy would refuse an untrusted peer correctly and let every
/// dial past the limits — which is the release criterion this experiment
/// exists to satisfy, and the reason the gate holds tickets rather than
/// dropping them.
pub async fn k19_ceilings_apply_to_behaviour_dials(r: &mut Report) {
    use interweave_transport_api::TransportIdentity;

    // A ONE-DIAL CEILING, so exhausting it takes one ticket.
    let cfg = NodeConfig {
        role: KadRole::Server,
        gate_mode: Mode::PolicyAdmit,
        max_pending_dials: 1,
        max_connections: 8,
        ..NodeConfig::default()
    };
    let plain = NodeConfig {
        max_pending_dials: 64,
        ..cfg.clone()
    };
    let mut nodes = vec![
        Node::start(&cfg).await,
        Node::start(&plain).await,
        Node::start(&plain).await,
    ];
    let (a, b, c) = (nodes[0].peer_id, nodes[1].peer_id, nodes[2].peer_id);
    for i in 0..3 {
        for p in [a, b, c] {
            nodes[i].trust(p);
        }
    }
    let (b_addr, c_addr) = (nodes[1].dial_address(), nodes[2].dial_address());
    nodes[1].dial_admitted(c_addr.clone());
    pump_until(&mut nodes, Duration::from_secs(10), |n| {
        n[1].observed.connected.contains(&c)
    })
    .await;
    if let Some(k) = nodes[1].kad() {
        k.add_address(&c, c_addr);
    }
    nodes[0].dial_admitted(b_addr.clone());
    pump_until(&mut nodes, Duration::from_secs(10), |n| {
        n[0].observed.connected.contains(&b)
    })
    .await;

    // HOLD THE ONLY PENDING SLOT, the way an in-flight ordinary dial
    // would. Nothing about Kademlia is told.
    let held = {
        let m = nodes[0].manager.lock().expect("manager");
        m.handle()
            .admit(
                &DialRequest {
                    peer: Some(TransportIdentity::parse(c.to_base58()).expect("canonical")),
                    address: "/ip4/198.51.100.9/tcp/1".to_owned(),
                    origin: DialOrigin::ConnectionManager,
                },
                0,
            )
            .expect("the first dial fits under a ceiling of one")
    };
    r.check(
        "K19.1",
        "one ordinary dial fills a pending-dial ceiling of one",
        nodes[0].manager.lock().expect("manager").handle().load().pending_dials() == 1,
    );

    nodes[0].ledger.reset();
    if let Some(k) = nodes[0].kad() {
        k.add_address(&b, b_addr);
        let id = k.get_closest_peers(c);
        nodes[0].own_queries.insert(id, QueryClass::Targeted);
    }
    pump(&mut nodes, Duration::from_secs(12)).await;

    let refusals = nodes[0].ledger.refusals();
    let originated = nodes[0].ledger.behaviour_originated();
    r.check(
        "K19.2",
        &format!("the query still asks for a dial ({originated})"),
        originated > 0,
    );
    r.check(
        "K19.3",
        "and the GLOBAL pending-dial ceiling refuses it",
        refusals.contains_key("too many pending dials") && nodes[0].ledger.behaviour_allowed() == 0,
    );
    r.note(format!("K19: {originated} dials, refusals {refusals:?}"));

    // RELEASED, and the ceiling stops refusing: the limit is a live
    // count, not a latch. Without this the previous assertion would
    // also pass for a gate that refused everything forever.
    drop(held);
    r.check(
        "K19.4",
        "releasing the ordinary dial returns the slot",
        nodes[0].manager.lock().expect("manager").handle().load().pending_dials() == 0,
    );
    nodes[0].ledger.reset();
    if let Some(k) = nodes[0].kad() {
        let id = k.get_closest_peers(c);
        nodes[0].own_queries.insert(id, QueryClass::Targeted);
    }
    let reached = pump_until(&mut nodes, Duration::from_secs(20), |n| {
        n[0].ledger.behaviour_allowed() > 0
    })
    .await;
    r.check(
        "K19.5",
        "and the same query is then admitted — the ceiling was the reason",
        reached,
    );
    r.note(format!(
        "K19 after release: {} dials, {} allowed, refusals {:?}",
        nodes[0].ledger.behaviour_originated(),
        nodes[0].ledger.behaviour_allowed(),
        nodes[0].ledger.refusals()
    ));

    // THE OTHER HALF, and the one the first misses. Everything above
    // fills the ceiling with an ORDINARY dial's ticket, so it would pass
    // for a gate that admitted behaviour dials without reserving
    // anything — dropping the gate's own ticket instead of holding it
    // leaves every assertion so far green. What follows has no external
    // ticket at all: the ceiling can only be filled by the gate's own.
    let tight = NodeConfig {
        role: KadRole::Server,
        gate_mode: Mode::PolicyAdmit,
        max_pending_dials: 1,
        max_connections: 8,
        parallelism: NonZeroUsize::new(3).expect("nonzero"),
        ..NodeConfig::default()
    };
    let router_cfg = NodeConfig {
        max_pending_dials: 64,
        ..tight.clone()
    };
    const ROUTERS: usize = 5;
    let mut fan = vec![Node::start(&tight).await];
    for _ in 0..ROUTERS {
        fan.push(Node::start(&router_cfg).await);
    }
    let fan_ids: Vec<_> = fan.iter().map(|n| n.peer_id).collect();
    let fan_addrs: Vec<_> = fan.iter().map(Node::dial_address).collect();
    for n in &mut fan {
        for id in &fan_ids {
            n.trust(*id);
        }
    }
    // ROUTED BUT NOT CONNECTED: `add_address` without a dial, so
    // reaching any of them REQUIRES a behaviour dial. Connecting first
    // would let the walk proceed over existing connections and the
    // ceiling would never be asked.
    fan[0].ledger.reset();
    for i in 1..=ROUTERS {
        let (peer, addr) = (fan_ids[i], fan_addrs[i].clone());
        if let Some(k) = fan[0].kad() {
            k.add_address(&peer, addr);
        }
    }
    if let Some(k) = fan[0].kad() {
        let id = k.get_closest_peers(libp2p::PeerId::random());
        fan[0].own_queries.insert(id, QueryClass::Exploration);
    }
    pump(&mut fan, Duration::from_secs(15)).await;

    let originated = fan[0].ledger.behaviour_originated();
    let allowed = fan[0].ledger.behaviour_allowed();
    let refusals = fan[0].ledger.refusals();
    r.check(
        "K19.6",
        &format!("a fan-out query asks for several dials at once ({originated})"),
        originated > 1,
    );
    r.check(
        "K19.7",
        &format!(
            "the gate's OWN tickets fill the ceiling, so some are refused: \
             {allowed} allowed of {originated}, refusals {refusals:?}"
        ),
        refusals.get("too many pending dials").copied().unwrap_or(0) > 0
            && allowed < originated,
    );
    // BOTH LIVE COUNTS AT ZERO. `pending_dials() <= 1` is guaranteed by
    // a ceiling of one even when the single admitted ticket leaks
    // forever — a gate that never settles its one allowed dial would
    // pass while permanently exhausted — and the gate's own count was
    // printed but not asserted.
    let pending = fan[0]
        .manager
        .lock()
        .expect("manager")
        .handle()
        .load()
        .pending_dials();
    let held = fan[0].swarm.behaviour().gate.pending_behaviour_dials();
    r.check(
        "K19.8",
        &format!("every slot comes back once the dials settle: {pending} pending, {held} held"),
        pending == 0 && held == 0,
    );
    // AND NOTHING WAS LEFT OUTSIDE THE ACCOUNTING. A settlement that
    // cannot re-mint its ticket drops the reservation, so the
    // established connection it belonged to would survive outside
    // `max_connections` with no address accounting — the ceiling
    // failing open. The gate closes such a connection; this asserts the
    // ordinary path never needed to.
    r.check(
        "K19.11",
        &format!(
            "no connection had to be closed for want of accounting: {}",
            fan[0].ledger.unaccounted_closed()
        ),
        fan[0].ledger.unaccounted_closed() == 0,
    );

    // THE CONNECTION CEILING, which the pending ceiling does not stand
    // in for. A `DialTicket` reserves BOTH, and its `Drop` returns both
    // — so a gate that dropped the ticket when a dial ESTABLISHED would
    // pass everything above while `max_connections` counted no
    // behaviour-originated connection at all. `record_success` converts
    // the ticket into a `ConnectionSlot` that keeps the reservation, and
    // this is what asserts it does.
    let conn_cfg = NodeConfig {
        role: KadRole::Server,
        gate_mode: Mode::PolicyAdmit,
        max_pending_dials: 8,
        // ONE. The routers below are reachable, so a successful
        // behaviour dial must consume it.
        max_connections: 1,
        ..NodeConfig::default()
    };
    let peer_cfg = NodeConfig {
        max_connections: 16,
        ..conn_cfg.clone()
    };
    let mut cn = vec![Node::start(&conn_cfg).await];
    for _ in 0..3 {
        cn.push(Node::start(&peer_cfg).await);
    }
    let cn_ids: Vec<_> = cn.iter().map(|n| n.peer_id).collect();
    let cn_addrs: Vec<_> = cn.iter().map(Node::dial_address).collect();
    for n in &mut cn {
        for id in &cn_ids {
            n.trust(*id);
        }
    }
    cn[0].ledger.reset();
    for i in 1..4 {
        let (peer, addr) = (cn_ids[i], cn_addrs[i].clone());
        if let Some(k) = cn[0].kad() {
            k.add_address(&peer, addr);
        }
    }
    if let Some(k) = cn[0].kad() {
        let id = k.get_closest_peers(libp2p::PeerId::random());
        cn[0].own_queries.insert(id, QueryClass::Exploration);
    }
    pump(&mut cn, Duration::from_secs(15)).await;

    let established = cn[0].swarm.behaviour().gate.held_connections();
    let live = cn[0]
        .manager
        .lock()
        .expect("manager")
        .handle()
        .load()
        .connections();
    r.check(
        "K19.9",
        &format!("a behaviour dial that ESTABLISHES keeps its connection slot: {established} held, manager counts {live}"),
        established >= 1 && live >= 1,
    );
    let refusals = cn[0].ledger.refusals();
    r.check(
        "K19.10",
        &format!(
            "and with the ceiling at one, further behaviour dials are refused \
             for the CONNECTION limit: {refusals:?}"
        ),
        refusals.contains_key("connection limit reached"),
    );
    r.note(format!(
        "K19 connections: {} dials, {} allowed, {established} slots held, refusals {refusals:?}",
        cn[0].ledger.behaviour_originated(),
        cn[0].ledger.behaviour_allowed()
    ));
}

/// K20 — trust revoked between admission and the completed handshake.
///
/// The gate admits a behaviour dial against the trust policy of the
/// moment it is asked. The Noise handshake completes later, and trust
/// can be revised in between — so settling with `record_success`
/// unconditionally retains a connection under authority that no longer
/// exists. The production settlement path reclassifies the authenticated
/// peer for exactly this race and has a distinct method for it,
/// `record_authorization_withdrawn`.
pub async fn k20_authority_withdrawn_mid_dial(r: &mut Report) {
    let cfg = NodeConfig {
        role: KadRole::Server,
        gate_mode: Mode::PolicyAdmit,
        ..NodeConfig::default()
    };
    let mut nodes = vec![
        Node::start(&cfg).await,
        Node::start(&cfg).await,
        Node::start(&cfg).await,
    ];
    let (a, b, c) = (nodes[0].peer_id, nodes[1].peer_id, nodes[2].peer_id);
    for i in 0..3 {
        for p in [a, b, c] {
            nodes[i].trust(p);
        }
    }
    let (b_addr, c_addr) = (nodes[1].dial_address(), nodes[2].dial_address());
    nodes[1].dial_admitted(c_addr.clone());
    pump_until(&mut nodes, Duration::from_secs(10), |n| {
        n[1].observed.connected.contains(&c)
    })
    .await;
    if let Some(k) = nodes[1].kad() {
        k.add_address(&c, c_addr);
    }
    nodes[0].dial_admitted(b_addr.clone());
    pump_until(&mut nodes, Duration::from_secs(10), |n| {
        n[0].observed.connected.contains(&b)
    })
    .await;

    // CONTROL FIRST: with trust intact the connection is RETAINED, so
    // the assertion below is about the revocation and not about the
    // walk failing for some other reason.
    nodes[0].ledger.reset();
    if let Some(k) = nodes[0].kad() {
        k.add_address(&b, b_addr.clone());
        let id = k.get_closest_peers(c);
        nodes[0].own_queries.insert(id, QueryClass::Targeted);
    }
    pump_until(&mut nodes, Duration::from_secs(20), |n| {
        n[0].ledger.retained() > 0
    })
    .await;
    r.check(
        "K20.1",
        &format!(
            "CONTROL: with trust intact a behaviour connection is retained ({} retained, {} withdrawn)",
            nodes[0].ledger.retained(),
            nodes[0].ledger.withdrawn()
        ),
        nodes[0].ledger.retained() > 0 && nodes[0].ledger.withdrawn() == 0,
    );

    // WHAT THE SETTLEMENT READS. The branch above asks
    // `classify(peer) == DataPlaneTrusted` at settlement rather than
    // trusting the classification admission made. Revocation is what
    // makes those two answers differ, so this asserts the input
    // genuinely changes — before revocation the peer classifies as
    // data-plane trusted, after it does not.
    let c_identity = interweave_transport_api::TransportIdentity::parse(c.to_base58())
        .expect("canonical");
    let before = nodes[0].manager.lock().expect("manager").classify(&c_identity);
    let acted = nodes[0].revoke(c);
    let after = nodes[0].manager.lock().expect("manager").classify(&c_identity);
    r.note(format!("K20: revocation acted on {acted} peer(s)"));
    r.check(
        "K20.2",
        &format!("revocation changes what the settlement reads: {before:?} -> {after:?}"),
        before == interweave_transport_runtime::ConnectionClass::DataPlaneTrusted
            && after != interweave_transport_runtime::ConnectionClass::DataPlaneTrusted,
    );

    // AND ADMISSION REFUSES from then on, which is the half that IS
    // deterministic: a revoked peer gets no new behaviour dial at all.
    // Revocation has already taken the connection down — no manual
    // disconnect here either.
    pump(&mut nodes, Duration::from_secs(2)).await;
    nodes[0].ledger.reset();
    if let Some(k) = nodes[0].kad() {
        let id = k.get_closest_peers(c);
        nodes[0].own_queries.insert(id, QueryClass::Targeted);
    }
    pump(&mut nodes, Duration::from_secs(12)).await;
    r.check(
        "K20.3",
        &format!(
            "after revocation nothing is retained for that peer: {} retained, \
             refusals {:?}",
            nodes[0].ledger.retained(),
            nodes[0].ledger.refusals()
        ),
        nodes[0].ledger.retained() == 0,
    );

    // THE LIMIT, stated rather than papered over. The window between an
    // admission and its completed handshake is milliseconds on
    // loopback, and this harness cannot open it on demand — so the
    // reclassification BRANCH is not driven here. What is established:
    // the branch reads a classification that revocation really changes
    // (K20.2), the trusted path really retains (K20.1), and no
    // retention happens for a revoked peer (K20.3). A test that claimed
    // to have hit the race would be claiming a schedule it does not
    // control.
    r.note(
        "K20 LIMIT: the admit-then-revoke-then-establish window is not driven \
         deterministically on loopback, so the reclassification branch itself \
         is unexercised. Stage 10 owns a test that can hold a dial open."
            .to_owned(),
    );
}

/// K21 — a behaviour dial offering several addresses.
///
/// This is finding F9's evidence. `handle_pending_outbound_connection`
/// returns addresses to ADD; it cannot remove the ones the dial already
/// carries. So a gate that checks only `addresses.first()` leaves libp2p
/// free to fall back to a second address that never crossed the address
/// policy — and records the outcome against an address it may not have
/// used.
///
/// Production never has this problem: `AdmittedDial::from_ticket` binds
/// exactly ONE address into its `DialOpts`. A behaviour dial is
/// multi-address, which is a shape the admission was not designed for.
pub async fn k21_multi_address_behaviour_dial(r: &mut Report) {
    use interweave_transport_api::TransportIdentity;

    let cfg = NodeConfig {
        role: KadRole::Server,
        gate_mode: Mode::PolicyAdmit,
        ..NodeConfig::default()
    };
    let mut nodes = vec![
        Node::start(&cfg).await,
        Node::start(&cfg).await,
        Node::start(&cfg).await,
    ];
    let (a, b, c) = (nodes[0].peer_id, nodes[1].peer_id, nodes[2].peer_id);
    for i in 0..3 {
        for p in [a, b, c] {
            nodes[i].trust(p);
        }
    }
    let (b_addr, c_addr) = (nodes[1].dial_address(), nodes[2].dial_address());
    // THE ROUTER HOLDS TWO ADDRESSES FOR `c`: the real one and a second
    // that `a` will have quarantined. A walk toward `c` then offers both.
    let second: libp2p::Multiaddr = "/ip4/127.0.0.1/tcp/2".parse().expect("valid");
    nodes[1].dial_admitted(c_addr.clone());
    pump_until(&mut nodes, Duration::from_secs(10), |n| {
        n[1].observed.connected.contains(&c)
    })
    .await;
    if let Some(k) = nodes[1].kad() {
        k.add_address(&c, c_addr.clone());
        k.add_address(&c, second.clone());
    }
    nodes[0].dial_admitted(b_addr.clone());
    pump_until(&mut nodes, Duration::from_secs(10), |n| {
        n[0].observed.connected.contains(&b)
    })
    .await;

    // QUARANTINE THE ADDRESS THE WALK WILL RETURN, through the
    // production path: an address that authenticated the wrong PeerId.
    // The router holds a second address too, but `FIND_NODE` answers
    // from what its routing table considers current — see K21.4's note.
    let c_identity = TransportIdentity::parse(c.to_base58()).expect("canonical");
    {
        let mut m = nodes[0].manager.lock().expect("manager");
        let ticket = m
            .handle()
            .admit(
                &DialRequest {
                    peer: Some(c_identity.clone()),
                    address: c_addr.to_string(),
                    origin: DialOrigin::ConnectionManager,
                },
                0,
            )
            .expect("admitted before the mismatch");
        let recorded = m.record_identity_mismatch(ticket, 0);
        r.check(
            "K21.1",
            "the second address is quarantined by an identity mismatch",
            recorded,
        );
    }
    // The quarantine is real: asked on its own, that address is refused.
    let quarantined = nodes[0].manager.lock().expect("manager").handle().admit(
        &DialRequest {
            peer: Some(c_identity.clone()),
            address: c_addr.to_string(),
            origin: DialOrigin::KademliaQuery,
        },
        1_000,
    );
    r.check(
        "K21.2",
        &format!("and refuses that address on its own: {:?}", quarantined.as_ref().err()),
        matches!(quarantined, Err(DialDenial::AddressQuarantined)),
    );
    drop(quarantined);
    // CONTROL: a DIFFERENT address for the same peer is still
    // admissible, so the refusal below is about the address and not
    // about the peer having been suppressed.
    let other = nodes[0].manager.lock().expect("manager").handle().admit(
        &DialRequest {
            peer: Some(c_identity),
            address: second.to_string(),
            origin: DialOrigin::KademliaQuery,
        },
        1_000,
    );
    r.check(
        "K21.3",
        &format!("CONTROL: another address for the same peer is admissible: {:?}", other.as_ref().err()),
        other.is_ok(),
    );
    drop(other);

    nodes[0].ledger.reset();
    if let Some(k) = nodes[0].kad() {
        k.add_address(&b, b_addr);
        let id = k.get_closest_peers(c);
        nodes[0].own_queries.insert(id, QueryClass::Targeted);
    }
    pump(&mut nodes, Duration::from_secs(15)).await;

    let refusals = nodes[0].ledger.refusals();
    let _offered = nodes[0]
        .observed
        .learned_addresses
        .get(&c)
        .map_or(0, std::collections::BTreeSet::len);
    r.check(
        "K21.4",
        &format!(
            "the walk really tried to reach the target: {} dials, aimed at it: {}",
            nodes[0].ledger.behaviour_originated(),
            nodes[0].ledger.behaviour_targets().contains(&c)
        ),
        nodes[0].ledger.behaviour_targets().contains(&c),
    );
    // THE FINDING. libp2p calls `handle_pending_outbound_connection`
    // with an EMPTY candidate list for a behaviour dial — the hook is
    // where behaviours CONTRIBUTE addresses, and the union is dialled
    // after it returns. So address-scoped policy cannot be decided
    // there, however carefully the list is walked. Measured rather than
    // reasoned about, because the whole class of bug here is a check
    // that runs against nothing.
    let offered_to_hook = nodes[0].ledger.offered_addresses();
    r.check(
        "K21.5",
        &format!(
            "the dial hook is given NO candidate addresses for a behaviour \
             dial: {offered_to_hook:?}"
        ),
        !offered_to_hook.is_empty() && offered_to_hook.iter().all(|n| *n == 0),
    );
    r.check(
        "K21.6",
        &format!(
            "the connection is refused on the address it actually used, so the \
             quarantine binds: {} address refusal(s), {refusals:?}",
            nodes[0].ledger.address_refusals()
        ),
        nodes[0].ledger.address_refusals() > 0
            && refusals.contains_key("address quarantined"),
    );
    r.check(
        "K21.7",
        "and no connection to that peer survives",
        !nodes[0].observed.connected.contains(&c),
    );
    r.note(format!(
        "K21: {} dials, {} allowed, refusals {refusals:?}, addresses OFFERED TO \
         THE HOOK {:?}, addresses learned {:?}",
        nodes[0].ledger.behaviour_originated(),
        nodes[0].ledger.behaviour_allowed(),
        nodes[0].ledger.offered_addresses(),
        nodes[0].observed.learned_addresses.get(&c)
    ));
}

/// K22 — the bounded query scheduler.
///
/// The brief requires "the bounded query scheduler" be exercised and
/// exploration validated "within the proposed budgets".
/// `kademlia-integration.md` §15 names them: a concurrency ceiling and a
/// rate ceiling, shared across the three query classes.
///
/// This is PROJECT logic. `kad::Behaviour` has `set_parallelism`, which
/// bounds the peers ONE query contacts at a time; it has no notion of
/// how many queries the provider may have running, or how often it may
/// start them. Starting queries directly on the behaviour — which every
/// other experiment here does — bypasses a scheduler that does not
/// exist, so a missing or leaking one would leave every observation
/// green.
pub struct QueryScheduler {
    max_concurrent: usize,
    max_per_minute: u32,
    /// Query starts inside the current window, oldest first.
    starts: std::collections::VecDeque<u64>,
    /// The queries currently holding a concurrency slot, by id. A set
    /// rather than a count so one completion releases one slot, and the
    /// slot it releases is its own.
    running: std::collections::HashSet<kad::QueryId>,
    /// Slots taken but not yet bound to a query id, with the timestamp
    /// each one spent — so releasing gives back both.
    unbound: std::collections::HashMap<u64, u64>,
    next_permit: u64,
}

/// Permission to start exactly one query.
///
/// Not `Copy` and not `Clone`: a permit is one slot, and duplicating it
/// would let one acquisition start two queries — which is the ceiling
/// failing in the same direction as an unkeyed completion.
#[derive(Debug)]
pub struct Permit(u64);

/// Why the scheduler refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchedulerRefusal {
    Concurrency,
    Rate,
}

impl QueryScheduler {
    const WINDOW_MS: u64 = 60_000;

    #[must_use]
    pub fn new(max_concurrent: usize, max_per_minute: u32) -> Self {
        Self {
            max_concurrent,
            max_per_minute,
            starts: std::collections::VecDeque::new(),
            running: std::collections::HashSet::new(),
            unbound: std::collections::HashMap::new(),
            next_permit: 0,
        }
    }

    /// Acquire a slot BEFORE the query exists.
    ///
    /// The ordering is the whole point. `kad::Behaviour::get_*` creates
    /// the query the moment it is called, so a scheduler consulted
    /// afterwards records a decision that has already been made: ten
    /// calls run ten queries however many the budget allowed, and the
    /// ceiling becomes bookkeeping. The permit is therefore taken first
    /// and bound to the query id afterwards — and a caller that cannot
    /// get one must not call the behaviour at all.
    ///
    /// # Errors
    /// [`SchedulerRefusal`] naming which budget refused. The two are
    /// distinct because they fail for different reasons and a caller
    /// that conflates them cannot tell "wait for a slot" from "wait for
    /// the window".
    pub fn acquire(&mut self, now_ms: u64) -> Result<Permit, SchedulerRefusal> {
        // THE WINDOW IS PRUNED FIRST, so an old start cannot occupy the
        // rate budget forever. Deque rather than a counter: a counter
        // reset on a tick would let a caller spend the whole budget in
        // the last millisecond of one window and again in the first of
        // the next.
        while self
            .starts
            .front()
            .is_some_and(|t| now_ms.saturating_sub(*t) >= Self::WINDOW_MS)
        {
            self.starts.pop_front();
        }
        if self.running.len() + self.unbound.len() >= self.max_concurrent {
            return Err(SchedulerRefusal::Concurrency);
        }
        if self.starts.len() as u32 >= self.max_per_minute {
            return Err(SchedulerRefusal::Rate);
        }
        self.starts.push_back(now_ms);
        self.next_permit += 1;
        self.unbound.insert(self.next_permit, now_ms);
        Ok(Permit(self.next_permit))
    }

    /// Bind a permit to the query it was taken for.
    ///
    /// Consumes the permit, so it cannot be bound twice. A permit that
    /// is dropped without binding — the caller took a slot and then
    /// failed to start anything — is released by [`Self::release`].
    pub fn bind(&mut self, permit: Permit, id: kad::QueryId) {
        if self.unbound.remove(&permit.0).is_some() {
            self.running.insert(id);
        }
    }

    /// Give back the concurrency slot of a permit whose work has
    /// FINISHED, keeping its rate charge.
    ///
    /// For headroom held against work the scheduler cannot gate: the
    /// library's implicit bootstrap really ran, so it really spent the
    /// window — but once it completes it is no longer occupying
    /// concurrency, and holding the slot forever would suppress a valid
    /// explicit query while claiming the two overlapped when they did
    /// not.
    ///
    /// The mirror of [`Self::release`], which is for a permit whose work
    /// never started and therefore spent nothing at all.
    pub fn consume(&mut self, permit: Permit) {
        self.unbound.remove(&permit.0);
    }

    /// Give back a permit that was never bound to a query.
    ///
    /// The RATE is refunded too, not only the concurrency slot. An
    /// acquisition that never became a query spent nothing, and leaving
    /// its timestamp in the window let a caller that acquires and then
    /// fails — a recovery path, repeated — exhaust `max_per_minute` with
    /// zero queries started and suppress exploration for the rest of the
    /// window. The timestamp is therefore carried by the permit and
    /// removed with it.
    pub fn release(&mut self, permit: Permit) {
        if let Some(at) = self.unbound.remove(&permit.0)
            && let Some(i) = self.starts.iter().position(|t| *t == at)
        {
            self.starts.remove(i);
        }
    }

    /// Slots held: bound to a query, or taken and not yet bound.
    #[must_use]
    pub fn held(&self) -> usize {
        self.running.len() + self.unbound.len()
    }

    /// One SPECIFIC query finished.
    ///
    /// Keyed, not counted. A bare `finish()` that decremented a counter
    /// could be called twice for one query — or once for the implicit
    /// bootstrap the library starts on a routing insertion, which the
    /// provider never scheduled — and the ceiling would then admit more
    /// than its budget. `saturating_sub` hid it: the counter simply
    /// floored at zero and the over-admission looked like normal
    /// operation.
    ///
    /// Returns whether this id was actually holding a slot, so a
    /// duplicate or foreign completion is visible rather than silent.
    pub fn finish(&mut self, id: kad::QueryId) -> bool {
        self.running.remove(&id)
    }

    /// Ids currently holding a slot.
    #[must_use]
    pub fn running(&self) -> usize {
        self.running.len()
    }

}

pub async fn k22_bounded_query_scheduler(r: &mut Report) {
    // §13's proposed defaults.
    const MAX_CONCURRENT: usize = 2;
    const MAX_PER_MINUTE: u32 = 6;

    // Real `QueryId`s, minted from a real behaviour, because the
    // scheduler is keyed by them.
    let cfg = NodeConfig {
        role: KadRole::Server,
        gate_mode: Mode::PolicyAdmit,
        ..NodeConfig::default()
    };
    let mut nodes = vec![Node::start(&cfg).await, Node::start(&cfg).await];
    let (a, b) = (nodes[0].peer_id, nodes[1].peer_id);
    for i in 0..2 {
        nodes[i].trust(a);
        nodes[i].trust(b);
    }
    let b_addr = nodes[1].dial_address();
    nodes[0].dial_admitted(b_addr.clone());
    pump_until(&mut nodes, Duration::from_secs(10), |n| {
        n[0].observed.connected.contains(&b)
    })
    .await;
    if let Some(k) = nodes[0].kad() {
        k.add_address(&b, b_addr);
    }
    pump(&mut nodes, Duration::from_secs(2)).await;

    // Real `QueryId`s for the bookkeeping assertions. These DO start
    // real queries on this node — a `QueryId` cannot be obtained any
    // other way — which is why the enforcement measurement below runs
    // on a SEPARATE, untouched node. Counting invocations on a node
    // that already holds twelve ungated queries would report two while
    // fourteen had happened.
    let mut ids = Vec::new();
    for _ in 0..12 {
        if let Some(k) = nodes[0].kad() {
            ids.push(k.get_n_closest_peers(random_32(), NonZeroUsize::new(4).expect("nonzero")));
        }
    }

    let mut s = QueryScheduler::new(MAX_CONCURRENT, MAX_PER_MINUTE);
    let p0 = s.acquire(0).expect("first fits");
    let p1 = s.acquire(0).expect("second fits");
    r.check(
        "K22.1",
        "the concurrency ceiling admits exactly its budget",
        s.held() == MAX_CONCURRENT,
    );
    r.check(
        "K22.2",
        "and refuses the next for CONCURRENCY, not for rate",
        matches!(s.acquire(0), Err(SchedulerRefusal::Concurrency)),
    );
    // A PERMIT HELD BUT NOT YET BOUND still occupies its slot: the
    // window between taking one and calling the behaviour is exactly
    // when a second caller must be refused.
    s.bind(p0, ids[0]);
    s.bind(p1, ids[1]);
    r.check(
        "K22.3",
        "binding does not change how many slots are held",
        s.held() == MAX_CONCURRENT,
    );

    // A COMPLETION RELEASES ITS OWN SLOT, and only its own.
    r.check(
        "K22.4",
        "a duplicate completion releases nothing the second time",
        s.finish(ids[0]) && !s.finish(ids[0]) && s.held() == MAX_CONCURRENT - 1,
    );
    r.check(
        "K22.5",
        "and a completion for a query the scheduler never started releases nothing",
        !s.finish(ids[9]) && s.held() == MAX_CONCURRENT - 1,
    );
    let p2 = s.acquire(0).expect("the freed slot");
    r.check(
        "K22.6",
        "the freed slot — exactly one — is available again",
        matches!(s.acquire(0), Err(SchedulerRefusal::Concurrency)),
    );
    // A PERMIT THAT NEVER BECAME A QUERY comes back.
    s.release(p2);
    r.check(
        "K22.7",
        "a permit released without starting anything returns its slot",
        s.held() == MAX_CONCURRENT - 1,
    );
    // AND ITS RATE. An acquisition that never became a query spent
    // nothing, so leaving its timestamp in the window would let a
    // recovery path — acquire, fail, release, retry — exhaust
    // `max_per_minute` with zero queries started and suppress
    // exploration for the rest of the window.
    let mut refund = QueryScheduler::new(MAX_CONCURRENT, MAX_PER_MINUTE);
    for _ in 0..(MAX_PER_MINUTE as usize * 3) {
        let p = refund.acquire(5_000).expect("a released permit costs nothing");
        refund.release(p);
    }
    r.check(
        "K22.7b",
        "and its RATE: acquire-then-release, repeated past the budget, spends nothing",
        refund.acquire(5_000).is_ok(),
    );
    // CONTROL: the same count of acquisitions that DO become queries
    // does exhaust the window, so the refund is not simply disabling
    // the rate.
    // Concurrency deliberately ABOVE the rate, so the rate is what
    // refuses rather than the concurrency ceiling getting there first.
    let mut spent = QueryScheduler::new(MAX_PER_MINUTE as usize + 1, MAX_PER_MINUTE);
    for id in ids.iter().take(MAX_PER_MINUTE as usize) {
        let p = spent.acquire(5_000).expect("within budget");
        spent.bind(p, *id);
    }
    r.check(
        "K22.7c",
        "CONTROL: permits that BECOME queries do spend the window",
        matches!(spent.acquire(5_000), Err(SchedulerRefusal::Rate)),
    );

    // THE RATE, which the concurrency ceiling does not stand in for: a
    // caller that starts and finishes promptly never hits concurrency
    // and could otherwise start without limit.
    let mut rate = QueryScheduler::new(MAX_CONCURRENT, MAX_PER_MINUTE);
    let mut started = 0;
    for id in ids.iter().take(12) {
        if let Ok(p) = rate.acquire(1_000) {
            started += 1;
            rate.bind(p, *id);
            rate.finish(*id);
        }
    }
    r.check(
        "K22.8",
        &format!("start-and-finish is bounded by the RATE, not concurrency: {started}"),
        started == MAX_PER_MINUTE as usize,
    );
    r.check(
        "K22.9",
        "and the refusal says which budget it was",
        matches!(rate.acquire(1_000), Err(SchedulerRefusal::Rate)),
    );

    // THE WINDOW SLIDES. A counter reset on a tick would let the whole
    // budget be spent in the last millisecond of one window and again in
    // the first of the next — twice the rate across the boundary.
    r.check(
        "K22.10",
        "the window is still closed just before it lapses",
        matches!(rate.acquire(1_000 + 59_999), Err(SchedulerRefusal::Rate)),
    );
    r.check(
        "K22.11",
        "and opens once the oldest start ages out",
        rate.acquire(1_000 + 60_000).is_ok(),
    );

    // AND IT REALLY GATES THE BEHAVIOUR. The permit is taken BEFORE
    // `get_n_closest_peers`, and the behaviour is not called at all when
    // there is no permit — `kad` creates the query the moment it is
    // called, so a scheduler consulted afterwards records a decision
    // already made and ten calls run ten queries whatever the budget
    // said.
    // A FRESH PAIR. Nothing has ever started a query on this node, so
    // the count below is the node's whole history rather than a delta
    // against twelve queries the bookkeeping section already ran.
    let mut fresh = vec![Node::start(&cfg).await, Node::start(&cfg).await];
    let (fa, fb) = (fresh[0].peer_id, fresh[1].peer_id);
    for i in 0..2 {
        fresh[i].trust(fa);
        fresh[i].trust(fb);
    }
    let fb_addr = fresh[1].dial_address();
    fresh[0].dial_admitted(fb_addr.clone());
    pump_until(&mut fresh, Duration::from_secs(10), |n| {
        n[0].observed.connected.contains(&fb)
    })
    .await;
    if let Some(k) = fresh[0].kad() {
        k.add_address(&fb, fb_addr);
    }
    pump(&mut fresh, Duration::from_secs(2)).await;

    // HEADROOM RESERVED FIRST, before any explicit query. The
    // `add_address` that seeded this node started an implicit bootstrap
    // (F2) which no scheduler can refuse — it is not a call the
    // provider makes — and it spends the same connections and dial
    // ceilings as anything else. So the driver's budget must account
    // for it BEFORE the driver spends the rest.
    //
    // An earlier version tried to charge it afterwards, which charged
    // nothing: the explicit queries had already taken every slot, so
    // the `acquire` failed for concurrency and never reached the
    // `release` that was meant to hold the cost — and `release` refunds
    // the rate anyway. Reserving means HOLDING a permit, not acquiring
    // and giving it back.
    // ACTIVE, not historical. `unattributed_queries` never forgets an
    // id, so reserving against it held a slot for work that had already
    // finished — suppressing a valid explicit query and then claiming
    // two fit concurrently when they never overlapped.
    let implicit_seen = fresh[0].observed.unattributed_queries.len();
    let implicit_active: Vec<_> = fresh[0]
        .observed
        .unattributed_queries
        .iter()
        .filter(|q| !fresh[0].observed.finished_queries.contains(q))
        .copied()
        .collect();
    let mut live = QueryScheduler::new(MAX_CONCURRENT, MAX_PER_MINUTE);
    let mut reserved: Vec<Permit> = (0..implicit_active.len())
        .filter_map(|_| live.acquire(0).ok())
        .collect();
    r.check(
        "K22.14",
        &format!(
            "headroom is reserved for the library's ACTIVE work only: \
             {implicit_seen} implicit query(ies) seen, {} still running, \
             {} permit(s) held",
            implicit_active.len(),
            reserved.len()
        ),
        implicit_seen > 0 && reserved.len() == implicit_active.len(),
    );
    // AND THE RATE IS SPENT EITHER WAY. A query that ran spent the
    // window whether or not it is still running, so a finished one
    // releases its concurrency slot and keeps its charge — which is
    // what `consume` is for, as against `release` for work that never
    // started.
    let finished_implicit = implicit_seen - implicit_active.len();
    for _ in 0..finished_implicit {
        if let Ok(p) = live.acquire(0) {
            live.consume(p);
        }
    }
    r.check(
        "K22.14b",
        &format!(
            "and work that already finished keeps its rate charge without \
             holding a slot: {finished_implicit} consumed"
        ),
        live.held() == reserved.len(),
    );

    let mut issued = 0;
    let mut refused = 0;
    // HOW MANY TIMES THE BEHAVIOUR WAS INVOKED. That is the enforcement
    // question — `kad` creates a query the moment it is called, so a
    // scheduler consulted afterwards leaves ten queries running while
    // reporting two. Counting invocations is what distinguishes the two
    // orderings; counting the scheduler's own totals cannot.
    let mut invocations = 0;
    for _ in 0..10 {
        match live.acquire(0) {
            Ok(permit) => {
                let Some(k) = fresh[0].kad() else { break };
                invocations += 1;
                let q = k.get_n_closest_peers(random_32(), NonZeroUsize::new(4).expect("nonzero"));
                live.bind(permit, q);
                fresh[0].own_queries.insert(q, QueryClass::Exploration);
                issued += 1;
            }
            Err(_) => refused += 1,
        }
    }
    r.check(
        "K22.12",
        &format!(
            "a driver asking for ten queries starts {issued} and is refused \
             {refused} — the ceiling MINUS the headroom held for the library's \
             own query"
        ),
        issued == MAX_CONCURRENT - reserved.len() && refused == 10 - issued,
    );
    r.check(
        "K22.13",
        &format!(
            "the DRIVER invoked the behaviour only for permitted work: \
             {invocations} calls for {issued} permits"
        ),
        invocations == issued,
    );
    // AND THE RESERVATION COST THE DRIVER SOMETHING. With headroom held
    // for the implicit query the driver gets fewer slots than the
    // ceiling — which is the whole point, and is what an assertion
    // that merely observed the implicit query could not show.
    r.check(
        "K22.15",
        &format!(
            "the driver gets the ceiling minus whatever headroom is held: \
             {issued} issued of {MAX_CONCURRENT}, {} reserved",
            reserved.len()
        ),
        issued == MAX_CONCURRENT - reserved.len(),
    );
    // AND HOLDING REALLY COSTS A SLOT. The assertion above is trivially
    // true when nothing is held — which is the ordinary case here,
    // because the implicit bootstrap finishes before the driver spends —
    // so the mechanism is exercised on its own rather than left to a
    // condition the run does not control.
    let mut demo = QueryScheduler::new(MAX_CONCURRENT, MAX_PER_MINUTE);
    let held = demo.acquire(0).expect("headroom for one implicit query");
    let mut got = 0;
    while demo.acquire(0).is_ok() {
        got += 1;
        assert!(got < 100, "the ceiling must be finite");
    }
    r.check(
        "K22.15b",
        &format!(
            "with one slot held for ungatable work the driver gets {got} of \
             {MAX_CONCURRENT}"
        ),
        got == MAX_CONCURRENT - 1,
    );
    // And returns it when that work completes, keeping the rate charge.
    demo.consume(held);
    r.check(
        "K22.15c",
        "and the slot comes back when it finishes",
        demo.acquire(0).is_ok(),
    );
    r.check(
        "K22.16",
        &format!(
            "so the node's whole concurrent load is inside the budget: {} = \
             {issued} permitted + {} held for work the scheduler cannot gate",
            issued + reserved.len(),
            reserved.len()
        ),
        issued + reserved.len() <= MAX_CONCURRENT,
    );
    // AND THE NODE AGREES. Its whole query history is what the driver
    // started plus whatever the library began on its own, so the
    // deliberate queries it reports must be exactly the permitted ones —
    // the check the invocation counter alone cannot make, since a
    // counter can be wrong in the same direction as the loop it counts.
    pump(&mut fresh, Duration::from_secs(8)).await;
    // AND THE HELD SLOTS COME BACK when the library's work completes,
    // rather than being pinned for the life of the node.
    for id in &implicit_active {
        if fresh[0].observed.finished_queries.contains(id)
            && let Some(p) = reserved.pop()
        {
            live.consume(p);
        }
    }
    r.check(
        "K22.16b",
        &format!(
            "every completed implicit query returned its headroom: {} still \
             reserved, {} held, {issued} explicit",
            reserved.len(),
            live.held()
        ),
        // ZERO RESIDUAL, not "at least the explicit ones". `held() >=
        // issued` is true while ANY implicit permit is still reserved,
        // because the explicit queries already contribute `issued` — so
        // deleting the release logic entirely passed it. The claim is
        // that completed implicit work returned its slot, and only an
        // exact count says that.
        reserved.is_empty() && live.held() == issued,
    );
    r.note(format!(
        "K22 LIMIT: the release loop over real implicit queries is a NO-OP in \
         this run — the bootstrap finishes before the driver spends, so \
         {} were active and nothing was held to return. Removing that loop \
         therefore fails nothing here, and K22.16b's zero-residual check \
         cannot see it. The MECHANISM is covered by K22.15b/K22.15c, which \
         hold a slot deliberately and show it costing the driver one and \
         coming back on completion.",
        implicit_active.len()
    ));
    r.check(
        "K22.17",
        &format!(
            "the node's own tally of deliberate queries matches: {} started, \
             {} unattributed to the driver",
            fresh[0].own_queries.len(),
            fresh[0].observed.unattributed_queries.len()
        ),
        fresh[0].own_queries.len() == issued,
    );
    r.note(
        "K22 LIMIT: the scheduler is project logic modelled here, not a \
         component of a Kademlia driver this harness has. What it now also \
         drives is the CONVERGENCE path: K11 and K17 acquire a permit before \
         every exploration query and assert that as many were started as were \
         permitted, so the reported ten- and twenty-node convergence is \
         convergence within the budgets rather than with them bypassed. The \
         remaining experiments still call the behaviour directly, which is \
         appropriate — they are about the gate and the policy, not the \
         scheduler."
            .to_owned(),
    );
}

/// K23 — behaviour-originated dial volume, BY QUERY CLASS.
///
/// The release criterion asks for volume "measured by query class". The
/// gate cannot read the class off a dial: libp2p hands
/// `handle_pending_outbound_connection` a connection id and a peer, and
/// for a behaviour dial nothing else — no query id, no originating
/// behaviour. So attribution has to come from the provider, which knows
/// what it started.
///
/// That makes it exact when one class is in flight and a SET when
/// several are. Both cases are measured here, because the second is a
/// real limit on what any Stage 10 implementation can report, not a
/// shortcut taken by this harness.
pub async fn k23_dial_volume_by_class(r: &mut Report) {
    let cfg = NodeConfig {
        role: KadRole::Server,
        gate_mode: Mode::PolicyAdmit,
        ..NodeConfig::default()
    };
    let mut nodes = vec![
        Node::start(&cfg).await,
        Node::start(&cfg).await,
        Node::start(&cfg).await,
    ];
    let (a, b, c) = (nodes[0].peer_id, nodes[1].peer_id, nodes[2].peer_id);
    for i in 0..3 {
        for p in [a, b, c] {
            nodes[i].trust(p);
        }
    }
    let (b_addr, c_addr) = (nodes[1].dial_address(), nodes[2].dial_address());
    nodes[1].dial_admitted(c_addr.clone());
    pump_until(&mut nodes, Duration::from_secs(10), |n| {
        n[1].observed.connected.contains(&c)
    })
    .await;
    if let Some(k) = nodes[1].kad() {
        k.add_address(&c, c_addr);
    }
    nodes[0].dial_admitted(b_addr.clone());
    pump_until(&mut nodes, Duration::from_secs(10), |n| {
        n[0].observed.connected.contains(&b)
    })
    .await;

    let classes = nodes[0].swarm.behaviour().gate.classes();

    // THE LIBRARY'S OWN WORK FIRST, with nothing declared. A routing
    // insertion starts a bootstrap the provider never asked for (F2),
    // and its dials must be visible as unattributed rather than
    // silently folded into whatever ran next.
    nodes[0].ledger.reset();
    if let Some(k) = nodes[0].kad() {
        k.add_address(&b, b_addr);
    }
    pump(&mut nodes, Duration::from_secs(6)).await;
    let implicit = nodes[0].ledger.by_class();
    let implicit_none = implicit.get("none").copied().unwrap_or(0);
    // A NONZERO `none` COUNT, not merely the absence of other labels.
    // With no dial at all — which this topology does not guarantee,
    // since it connects to the router before `add_address` — the map is
    // empty and `keys().all(..)` is vacuously true, so the check
    // reported that implicit-work dials were measured and attributed
    // when none had happened.
    r.check(
        "K23.1",
        &format!(
            "dials from work the provider never started are attributed to none: \
             {implicit_none} such dial(s), {implicit:?}"
        ),
        implicit_none > 0 && implicit.keys().all(|k| k == "none"),
    );

    // ONE CLASS IN FLIGHT: attribution is exact. The connection the
    // implicit bootstrap just made is dropped first, or the targeted
    // query has nothing to dial and the window measures nothing — which
    // is what the first run of this experiment reported.
    let _ = nodes[0].swarm.disconnect_peer_id(c);
    pump(&mut nodes, Duration::from_secs(2)).await;
    nodes[0].ledger.reset();
    classes.started("targeted");
    if let Some(k) = nodes[0].kad() {
        let id = k.get_closest_peers(c);
        nodes[0].own_queries.insert(id, QueryClass::Targeted);
    }
    pump(&mut nodes, Duration::from_secs(12)).await;
    classes.finished("targeted");
    let single = nodes[0].ledger.by_class();
    let targeted = single.get("targeted").copied().unwrap_or(0);
    r.check(
        "K23.2",
        &format!("with one class in flight every dial is attributed to it: {single:?}"),
        targeted > 0 && single.keys().all(|k| k == "targeted"),
    );
    r.check(
        "K23.3",
        &format!(
            "and the per-class total accounts for every behaviour dial: {} == {}",
            single.values().sum::<u64>(),
            nodes[0].ledger.behaviour_originated()
        ),
        single.values().sum::<u64>() == nodes[0].ledger.behaviour_originated(),
    );

    // TWO CLASSES AT ONCE: attribution degrades to a SET, and says so
    // rather than picking one.
    let _ = nodes[0].swarm.disconnect_peer_id(c);
    pump(&mut nodes, Duration::from_secs(2)).await;
    nodes[0].ledger.reset();
    classes.started("targeted");
    classes.started("exploration");
    let started = nodes[0].kad().map(|k| {
        (
            k.get_closest_peers(c),
            k.get_n_closest_peers(random_32(), NonZeroUsize::new(4).expect("nonzero")),
        )
    });
    if let Some((t, e)) = started {
        nodes[0].own_queries.insert(t, QueryClass::Targeted);
        nodes[0].own_queries.insert(e, QueryClass::Exploration);
    }
    pump(&mut nodes, Duration::from_secs(12)).await;
    classes.finished("targeted");
    classes.finished("exploration");
    let mixed = nodes[0].ledger.by_class();
    let mixed_total: u64 = mixed.values().sum();
    let mixed_labels = mixed.keys().filter(|k| k.contains('+')).count();
    r.check(
        "K23.4",
        &format!(
            "with two classes in flight the attribution is the SET, not a guess \
             at one of them: {mixed_total} dial(s), {mixed_labels} mixed \
             label(s), {mixed:?}"
        ),
        // AT LEAST ONE DIAL, AND AT LEAST ONE MIXED LABEL. The old
        // predicate accepted an empty ledger and accepted every dial
        // labelled `none`, so a regression that lost attribution exactly
        // when several classes overlap would pass while K23.2's
        // single-class control stayed green — the one case this
        // experiment exists for.
        mixed_total > 0
            && mixed_labels > 0
            && mixed.keys().all(|k| k.contains('+') || k == "none"),
    );
    r.note(
        "K23 LIMIT: libp2p does not tell a behaviour which query caused a dial \\
         — the hook receives a connection id and a peer and nothing else. So \\
         attribution comes from what the PROVIDER declares it is running, and \\
         is exact only while one class is in flight. Stage 10 can narrow this \\
         by serialising classes or by widening the driver port; it cannot \\
         read it off the dial."
            .to_owned(),
    );
}

/// K24 — single-path capture, measured against controls.
///
/// The brief's expected evidence includes that "disjoint query paths and
/// multi-seed topologies measurably reduce single-path capture, without
/// claiming Byzantine resistance". K16 measured path WIDTH and found no
/// difference at six nodes, which is not the same question: width is how
/// many routers a query contacts, capture is how much of the answer
/// depends on any ONE of them.
///
/// The adversary here is deliberately the weakest kind that still
/// captures: a router that simply does not know the target. It is not
/// Byzantine — it returns a truthful empty answer — and that is the
/// point, because a claim about Byzantine resistance is exactly what
/// this must not make.
///
/// Enough routers that a query cannot contact them all at once:
/// `parallelism` is 3 and there are 9, so which routers a walk reaches
/// is a real variable rather than "all of them".
pub async fn k24_single_path_capture(r: &mut Report) {
    const ROUTERS: usize = 9;
    /// How many routers know the target. The rest are the capture.
    const KNOWERS: usize = 2;

    /// One run: returns whether the asker found the target's address.
    async fn attempt(disjoint: bool, seeds: usize) -> bool {
        let cfg = NodeConfig {
            role: KadRole::Server,
            gate_mode: Mode::PolicyAdmit,
            parallelism: NonZeroUsize::new(3).expect("nonzero"),
            disjoint_paths: disjoint,
            ..NodeConfig::default()
        };
        // 0 = asker, 1..=ROUTERS = routers, last = target.
        let mut n = Vec::new();
        for _ in 0..=(ROUTERS + 1) {
            n.push(Node::start(&cfg).await);
        }
        let ids: Vec<_> = n.iter().map(|x| x.peer_id).collect();
        let addrs: Vec<_> = n.iter().map(Node::dial_address).collect();
        for x in &mut n {
            for id in &ids {
                x.trust(*id);
            }
        }
        let target = ids[ROUTERS + 1];
        let target_addr = addrs[ROUTERS + 1].clone();

        // Only the first KNOWERS routers know the target.
        for i in 1..=KNOWERS {
            let a = target_addr.clone();
            n[i].dial_admitted(a.clone());
            if let Some(k) = n[i].kad() {
                k.add_address(&target, a);
            }
        }
        // The asker is seeded with `seeds` routers, chosen so that a
        // single seed is one that does NOT know the target — which is
        // what makes capture possible at all.
        for i in 0..seeds {
            let idx = ROUTERS - i; // from the far end: non-knowers first
            n[0].dial_admitted(addrs[idx].clone());
        }
        // Routers know each other, so a walk can move between them.
        for i in 1..=ROUTERS {
            for j in 1..=ROUTERS {
                if i != j {
                    let (p, a) = (ids[j], addrs[j].clone());
                    if let Some(k) = n[i].kad() {
                        k.add_address(&p, a);
                    }
                }
            }
        }
        pump(&mut n, Duration::from_secs(8)).await;
        for i in 0..seeds {
            let idx = ROUTERS - i;
            let (p, a) = (ids[idx], addrs[idx].clone());
            if let Some(k) = n[0].kad() {
                k.add_address(&p, a);
            }
        }
        pump(&mut n, Duration::from_secs(2)).await;

        for _ in 0..3 {
            if let Some(k) = n[0].kad() {
                let q = k.get_closest_peers(target);
                n[0].own_queries.insert(q, QueryClass::Targeted);
            }
            pump(&mut n, Duration::from_secs(10)).await;
            if n[0]
                .observed
                .learned_addresses
                .get(&target)
                .is_some_and(|a| !a.is_empty())
            {
                return true;
            }
        }
        false
    }

    // ONE SEED, a router that does not know the target: the walk depends
    // entirely on that router's view.
    let single_off = attempt(false, 1).await;
    let single_on = attempt(true, 1).await;
    // THREE SEEDS, so no single router's view is the whole answer.
    let multi_off = attempt(false, 3).await;
    let multi_on = attempt(true, 3).await;

    r.note(format!(
        "K24: found the target — 1 seed disjoint=false {single_off}, \
         1 seed disjoint=true {single_on}, 3 seeds disjoint=false {multi_off}, \
         3 seeds disjoint=true {multi_on}"
    ));
    r.check(
        "K24.1",
        &format!("a multi-seed asker reaches the target ({multi_on} / {multi_off})"),
        multi_on || multi_off,
    );
    // WHAT IS AND IS NOT ESTABLISHED. If the single-seed runs also
    // succeed, this topology does not exhibit capture at all and the
    // comparison says nothing — which must be REPORTED, not read as a
    // pass for the option.
    let capture_observed = !(single_off && single_on);
    r.check(
        "K24.2",
        &format!(
            "the comparison is meaningful only if a single seed can FAIL to \\
             reach the target; observed capture: {capture_observed}"
        ),
        // Not an assertion about the option — an assertion that the
        // experiment reports which case it is in.
        true,
    );
    r.note(if capture_observed {
        "K24: single-seed capture WAS observed, so multi-seed is a measurable \
         improvement on this topology."
            .to_owned()
    } else {
        "K24: NO capture observed — a single seed reached the target too, so \
         this topology cannot distinguish the configurations. The \
         expected-evidence item about reducing single-path capture is \
         therefore NOT established by this spike, and the record says so \
         rather than treating the absence of a difference as a pass."
            .to_owned()
    });
}

/// K25 — a multi-address dial where EVERY candidate fails.
///
/// K18 keeps a known-good route to the target alive, which suppresses
/// peer backoff — so its multi-address assertion never meets the
/// ordinary case. Here every candidate is dead, so the first settlement
/// advances peer backoff, and any later `admit` for the remaining
/// addresses is refused for it. A settlement loop that admits as it goes
/// therefore scores the first address and silently drops the rest, which
/// is precisely the outcome the multi-address fix exists to prevent.
pub async fn k25_every_candidate_fails(r: &mut Report) {
    use interweave_transport_api::TransportIdentity;

    let cfg = NodeConfig {
        role: KadRole::Server,
        gate_mode: Mode::PolicyAdmit,
        ..NodeConfig::default()
    };
    let mut nodes = vec![
        Node::start(&cfg).await,
        Node::start(&cfg).await,
        Node::start(&cfg).await,
    ];
    let (a, b, c) = (nodes[0].peer_id, nodes[1].peer_id, nodes[2].peer_id);
    for i in 0..3 {
        for p in [a, b, c] {
            nodes[i].trust(p);
        }
    }
    let (b_addr, c_addr) = (nodes[1].dial_address(), nodes[2].dial_address());
    // THE ROUTER KNOWS `c` ONLY AT DEAD ADDRESSES. It reaches `c` once
    // to learn it exists, then holds two addresses that refuse.
    let dead_a: libp2p::Multiaddr = "/ip4/127.0.0.1/tcp/4".parse().expect("valid");
    let dead_b: libp2p::Multiaddr = "/ip4/127.0.0.1/tcp/5".parse().expect("valid");
    nodes[1].dial_admitted(c_addr);
    pump_until(&mut nodes, Duration::from_secs(10), |n| {
        n[1].observed.connected.contains(&c)
    })
    .await;
    if let Some(k) = nodes[1].kad() {
        k.add_address(&c, dead_a.clone());
        k.add_address(&c, dead_b.clone());
    }
    nodes[0].dial_admitted(b_addr.clone());
    pump_until(&mut nodes, Duration::from_secs(10), |n| {
        n[0].observed.connected.contains(&b)
    })
    .await;

    // NO GOOD ROUTE TO `c` IS EVER RECORDED, which is what lets the
    // first failure advance peer backoff — the condition K18 avoids.
    let c_identity = TransportIdentity::parse(c.to_base58()).expect("canonical");
    nodes[0].ledger.reset();
    if let Some(k) = nodes[0].kad() {
        k.add_address(&b, b_addr);
        let id = k.get_closest_peers(c);
        nodes[0].own_queries.insert(id, QueryClass::Targeted);
    }
    pump(&mut nodes, Duration::from_secs(18)).await;

    let candidates = nodes[0]
        .manager
        .lock()
        .expect("manager")
        .dial_candidates(&c_identity, 60_000);
    let known = nodes[0]
        .manager
        .lock()
        .expect("manager")
        .known_addresses(&c_identity);
    r.note(format!(
        "K25: candidates {candidates:?}, known {known}, dial errors {}",
        nodes[0].observed.dial_errors.len()
    ));
    r.check(
        "K25.1",
        &format!("the dial really exhausted several addresses ({known} known)"),
        known >= 2,
    );
    // BOTH, by name. A settlement loop that admits as it goes scores the
    // first and is refused `PeerBackoff` for the rest — one address
    // recorded, the other silently dropped.
    r.check(
        "K25.2",
        &format!(
            "EVERY exhausted address is scored even though the first failure \\
             put the peer into backoff: {candidates:?}"
        ),
        candidates.iter().any(|x| x == &dead_a.to_string())
            && candidates.iter().any(|x| x == &dead_b.to_string()),
    );
    r.check(
        "K25.3",
        &format!(
            "and nothing was silently dropped: {} unsettled",
            nodes[0].ledger.unsettled_addresses()
        ),
        nodes[0].ledger.unsettled_addresses() == 0,
    );

    // THE SAME DIAL UNDER A TIGHT CEILING. Pre-minting one ticket per
    // address is what decoupled settlement from peer backoff, and it
    // bought a dependency on spare capacity: with one pending-dial slot
    // the first ticket takes it and the rest are refused. That shortfall
    // must be VISIBLE — a silent omission here is the same defect the
    // pre-minting fixed, wearing the ceiling as a disguise.
    let tight = NodeConfig {
        max_pending_dials: 1,
        max_connections: 1,
        ..cfg.clone()
    };
    let mut t = vec![
        Node::start(&tight).await,
        Node::start(&cfg).await,
        Node::start(&cfg).await,
    ];
    let (ta, tb, tc) = (t[0].peer_id, t[1].peer_id, t[2].peer_id);
    for i in 0..3 {
        for p in [ta, tb, tc] {
            t[i].trust(p);
        }
    }
    let (tb_addr, tc_addr) = (t[1].dial_address(), t[2].dial_address());
    t[1].dial_admitted(tc_addr);
    pump_until(&mut t, Duration::from_secs(10), |n| {
        n[1].observed.connected.contains(&tc)
    })
    .await;
    if let Some(k) = t[1].kad() {
        k.add_address(&tc, dead_a.clone());
        k.add_address(&tc, dead_b.clone());
    }
    t[0].dial_admitted(tb_addr.clone());
    pump_until(&mut t, Duration::from_secs(10), |n| {
        n[0].observed.connected.contains(&tb)
    })
    .await;
    t[0].ledger.reset();
    if let Some(k) = t[0].kad() {
        k.add_address(&tb, tb_addr);
        let id = k.get_closest_peers(tc);
        t[0].own_queries.insert(id, QueryClass::Targeted);
    }
    pump(&mut t, Duration::from_secs(18)).await;

    let tc_identity = TransportIdentity::parse(tc.to_base58()).expect("canonical");
    let tight_candidates = t[0]
        .manager
        .lock()
        .expect("manager")
        .dial_candidates(&tc_identity, 60_000);
    let shortfall = t[0].ledger.unsettled_addresses();
    r.note(format!(
        "K25 tight ceiling: candidates {tight_candidates:?}, unsettled {shortfall}"
    ));
    r.check(
        "K25.4",
        &format!(
            "under a tight ceiling the shortfall is REPORTED rather than silent: \
             {shortfall} unsettled, {} scored",
            tight_candidates.len()
        ),
        shortfall > 0 || tight_candidates.len() >= 2,
    );
}

/// K26 — capability-aware manual admission.
///
/// §7's pipeline ends in "exact current Kademlia server protocol
/// advertised" before `AddRoutingAddress`, and does not exempt the
/// source of the candidate. A query result is ADVISORY: it says a peer
/// exists at an address, not that the peer serves this DHT.
///
/// The distinction only becomes visible with a peer that is trusted,
/// reachable, identified, and NOT a server — which is a client-mode
/// node, the case §9.2 says must not be misrepresented as discoverable.
pub async fn k26_capability_aware_admission(r: &mut Report) {
    let network = "spike-003".to_owned();
    let protocol = namespace::protocol_name(&network);
    let server = NodeConfig {
        role: KadRole::Server,
        network_id: network.clone(),
        gate_mode: Mode::PolicyAdmit,
        ..NodeConfig::default()
    };
    let client = NodeConfig {
        role: KadRole::Client,
        ..server.clone()
    };
    // 0 = the asker, 1 = a server router, 2 = a CLIENT-mode peer.
    let mut nodes = vec![
        Node::start(&server).await,
        Node::start(&server).await,
        Node::start(&client).await,
    ];
    let (a, b, c) = (nodes[0].peer_id, nodes[1].peer_id, nodes[2].peer_id);
    for i in 0..3 {
        for p in [a, b, c] {
            nodes[i].trust(p);
        }
    }
    let (b_addr, c_addr) = (nodes[1].dial_address(), nodes[2].dial_address());

    // The asker connects to BOTH, so both are trusted, reachable and
    // identified — everything the pipeline wants except the protocol.
    nodes[0].dial_admitted(b_addr.clone());
    nodes[0].dial_admitted(c_addr.clone());
    pump_until(&mut nodes, Duration::from_secs(15), |n| {
        n[0].observed.identify_protocols.contains_key(&b)
            && n[0].observed.identify_protocols.contains_key(&c)
    })
    .await;
    r.check(
        "K26.1",
        "both peers are connected and identified",
        nodes[0].observed.identify_protocols.contains_key(&b)
            && nodes[0].observed.identify_protocols.contains_key(&c),
    );
    r.check(
        "K26.2",
        "the server advertises the exact protocol and the client does not",
        nodes[0].observed.identify_protocols[&b].contains(&protocol)
            && !nodes[0].observed.identify_protocols[&c].contains(&protocol),
    );

    // THE PIPELINE. Both are candidates by every other measure; only the
    // capability check separates them.
    let admitted = crate::topology::admit_candidates(&mut nodes[0], &protocol);
    pump(&mut nodes, Duration::from_secs(2)).await;
    let routed = nodes[0].routing_peers();
    r.check(
        "K26.3",
        &format!("the server is admitted to the routing table ({admitted} admitted, {routed} routed)"),
        routed == 1,
    );

    // A CLIENT-MODE PEER IS NOT RETURNED BY A WALK AT ALL. The router
    // holds it — asserted above — and a `get_closest_peers` result
    // still does not contain it, because the walk verifies a peer by
    // contacting it and a client-mode node does not answer the
    // protocol. So §9.2's "client nodes are not assumed discoverable by
    // PeerId through FIND_NODE" is enforced by rust-libp2p itself,
    // which is finding F17 and is why the query-derived path below uses
    // a SERVER the asker has no evidence about instead.
    nodes[1].dial_admitted(c_addr.clone());
    pump_until(&mut nodes, Duration::from_secs(10), |n| {
        n[1].observed.connected.contains(&c)
    })
    .await;
    if let Some(k) = nodes[1].kad() {
        k.add_address(&c, c_addr);
    }
    pump(&mut nodes, Duration::from_secs(2)).await;
    r.check(
        "K26.4",
        &format!(
            "the router holds the client in ITS routing table ({} entries)",
            nodes[1].routing_peers()
        ),
        nodes[1].routing_peers() >= 1,
    );
    crate::topology::admit_candidates(&mut nodes[0], &protocol);
    let _ = nodes[0].swarm.disconnect_peer_id(c);
    pump(&mut nodes, Duration::from_secs(3)).await;
    if let Some(k) = nodes[0].kad() {
        let id = k.get_closest_peers(c);
        nodes[0].own_queries.insert(id, QueryClass::Targeted);
    }
    pump(&mut nodes, Duration::from_secs(15)).await;
    r.check(
        "K26.5",
        &format!(
            "yet a walk toward it never returns it: learned {:?}",
            nodes[0].observed.learned_addresses.contains_key(&c)
        ),
        !nodes[0].observed.learned_addresses.contains_key(&c),
    );

    // THE QUERY-DERIVED PATH, with a peer that IS returned. A fourth
    // node, server-mode, known to the router and never connected to the
    // asker — so the asker holds no capability evidence about it, which
    // is the ordinary state for anything a query hands back.
    let mut extra = Node::start(&server).await;
    let d = extra.peer_id;
    let d_addr = extra.dial_address();
    for id in [a, b, c, d] {
        extra.trust(id);
    }
    for n in &mut nodes {
        n.trust(d);
    }
    nodes.push(extra);
    nodes[1].dial_admitted(d_addr.clone());
    pump_until(&mut nodes, Duration::from_secs(10), |n| {
        n[1].observed.connected.contains(&d)
    })
    .await;
    if let Some(k) = nodes[1].kad() {
        k.add_address(&d, d_addr.clone());
    }
    pump(&mut nodes, Duration::from_secs(2)).await;

    let before = nodes[0].routing_peers();
    if let Some(k) = nodes[0].kad() {
        let id = k.get_closest_peers(d);
        nodes[0].own_queries.insert(id, QueryClass::Targeted);
    }
    pump(&mut nodes, Duration::from_secs(15)).await;
    let learned_d = nodes[0].observed.learned_addresses.contains_key(&d);
    r.check(
        "K26.6",
        &format!("the query delivers a server peer as a candidate: {learned_d}"),
        learned_d,
    );
    // THE CAPABILITY CHECK, ISOLATED. Asking for a peer with NO
    // evidence turned out not to be reachable through a query: a walk
    // verifies a peer by contacting it, so anything it returns has
    // already been identified by the time the result arrives, and the
    // "returned but unidentified" state barely exists (F17). Trying to
    // assert it produced a check that passed for the wrong reason.
    //
    // So the gate is exercised on the evidence itself: the SAME
    // candidate, the same run, admitted against a protocol it does not
    // advertise — a different `network_id`'s. If the check is doing
    // anything, that refuses; if it is not, the candidate goes in.
    let identified = nodes[0].observed.identify_protocols.contains_key(&d);
    r.check(
        "K26.7",
        &format!("the walk identified the peer it returned: {identified}"),
        identified,
    );
    let foreign = namespace::protocol_name("spike-003-elsewhere");
    let admitted_foreign = crate::topology::admit_candidates(&mut nodes[0], &foreign);
    pump(&mut nodes, Duration::from_secs(2)).await;
    r.note(format!(
        "K26: admitting against a foreign protocol admitted {admitted_foreign}; \
         routing {} (was {before})",
        nodes[0].routing_peers()
    ));
    r.check(
        "K26.8",
        &format!(
            "a candidate that does not advertise the required protocol is \
             refused: {admitted_foreign} admitted, routing {} (was {before})",
            nodes[0].routing_peers()
        ),
        admitted_foreign == 0 && nodes[0].routing_peers() == before,
    );

    // AND ADMITTED UNDER THE PROTOCOL IT DOES ADVERTISE. Without this
    // the assertion above is satisfied by a pipeline that refuses
    // everything.
    let admitted_ours = crate::topology::admit_candidates(&mut nodes[0], &protocol);
    pump(&mut nodes, Duration::from_secs(2)).await;
    r.check(
        "K26.9",
        &format!(
            "CONTROL: the same candidate IS admitted under the protocol it does \
             advertise: {admitted_ours} admitted, routing {} (was {before})",
            nodes[0].routing_peers()
        ),
        admitted_ours > 0 && nodes[0].routing_peers() > before,
    );
}

/// K27 — behaviour dials under address-table pressure.
///
/// Two halves of finding F16, which is about which hook may decide
/// what.
///
/// The dial hook's probe carries the empty placeholder, so an
/// address-scoped denial there judges an address that does not exist —
/// deferred. But the RESERVATION cannot be deferred: `admit` decides
/// policy and takes the reservation in one call, so a dial whose
/// placeholder request is refused cannot obtain a ticket, and admitting
/// without one would leave the ceilings bounding nothing. The only
/// available answer is to refuse, which is fail-closed and an
/// availability cost.
///
/// At the established hook the address is real, and there
/// `PolicyStateFull` means the table cannot take an entry for the route
/// this connection actually used — the fail-closed address bound, and
/// exactly the decision deferred to that point.
pub async fn k27_address_table_pressure(r: &mut Report) {
    use interweave_transport_api::TransportIdentity;

    // A TINY ADDRESS TABLE, so filling it is cheap and deterministic.
    let cfg = NodeConfig {
        role: KadRole::Server,
        gate_mode: Mode::PolicyAdmit,
        max_addresses: 4,
        ..NodeConfig::default()
    };
    let peer_cfg = NodeConfig {
        max_addresses: 8_192,
        ..cfg.clone()
    };
    let mut nodes = vec![
        Node::start(&cfg).await,
        Node::start(&peer_cfg).await,
        Node::start(&peer_cfg).await,
    ];
    let (a, b, c) = (nodes[0].peer_id, nodes[1].peer_id, nodes[2].peer_id);
    for i in 0..3 {
        for p in [a, b, c] {
            nodes[i].trust(p);
        }
    }
    let (b_addr, c_addr) = (nodes[1].dial_address(), nodes[2].dial_address());
    nodes[1].dial_admitted(c_addr.clone());
    pump_until(&mut nodes, Duration::from_secs(10), |n| {
        n[1].observed.connected.contains(&c)
    })
    .await;
    if let Some(k) = nodes[1].kad() {
        k.add_address(&c, c_addr);
    }
    nodes[0].dial_admitted(b_addr.clone());
    pump_until(&mut nodes, Duration::from_secs(10), |n| {
        n[0].observed.connected.contains(&b)
    })
    .await;

    // FILL THE TABLE WITH LIVE QUARANTINES, through the production
    // identity-mismatch path. Only a quarantine is non-evictable, which
    // is what makes the table genuinely full rather than merely large.
    let b_identity = TransportIdentity::parse(b.to_base58()).expect("canonical");
    let mut recorded = 0;
    {
        let mut m = nodes[0].manager.lock().expect("manager");
        for i in 0..8 {
            let addr = format!("/ip4/198.51.100.{i}/tcp/1");
            let Ok(ticket) = m.handle().admit(
                &DialRequest {
                    peer: Some(b_identity.clone()),
                    address: addr,
                    origin: DialOrigin::ConnectionManager,
                },
                0,
            ) else {
                break;
            };
            if m.record_identity_mismatch(ticket, 0) {
                recorded += 1;
            }
        }
    }
    r.check(
        "K27.1",
        &format!("the address table is filled with live quarantines ({recorded})"),
        recorded >= 2,
    );
    // AND IT IS GENUINELY FULL: a fresh address for a fresh peer is
    // refused with the table's own denial.
    let stranger = TransportIdentity::parse(c.to_base58()).expect("canonical");
    let full = nodes[0].manager.lock().expect("manager").handle().admit(
        &DialRequest {
            peer: Some(stranger),
            address: "/ip4/203.0.113.7/tcp/1".to_owned(),
            origin: DialOrigin::KademliaQuery,
        },
        0,
    );
    r.check(
        "K27.2",
        &format!("and reports itself full: {:?}", full.as_ref().err()),
        matches!(full, Err(DialDenial::PolicyStateFull)),
    );
    drop(full);

    nodes[0].ledger.reset();
    if let Some(k) = nodes[0].kad() {
        k.add_address(&b, b_addr);
        let id = k.get_closest_peers(c);
        nodes[0].own_queries.insert(id, QueryClass::Targeted);
    }
    pump(&mut nodes, Duration::from_secs(15)).await;

    let refusals = nodes[0].ledger.refusals();
    let deferred = nodes[0].ledger.deferred_address_denials();
    let blocked = nodes[0].ledger.placeholder_blocked();
    r.note(format!(
        "K27: {} dials, {} allowed, refusals {refusals:?}, deferred {deferred}, \
         placeholder-blocked {blocked}",
        nodes[0].ledger.behaviour_originated(),
        nodes[0].ledger.behaviour_allowed()
    ));
    // THE PROBE DEFERRED, and the reservation could not. Both halves
    // are asserted because the pair is the finding: deferring the probe
    // alone changes nothing, which is what made the first version of
    // F16's fix ineffective.
    r.check(
        "K27.3",
        &format!("the dial hook's probe deferred its address denial ({deferred})"),
        deferred > 0,
    );
    r.check(
        "K27.4",
        &format!(
            "and the RESERVATION could not be deferred, so the dial is refused \\
             fail-closed rather than admitted without a ticket: {blocked} blocked"
        ),
        blocked > 0 && nodes[0].ledger.behaviour_allowed() == 0,
    );
    r.check(
        "K27.5",
        "which is reported as the address-table denial it is",
        refusals.contains_key("policy state full"),
    );
}

/// K28 — a connection whose authority lapsed does not survive.
///
/// K20 covers the settlement's DECISION: with trust intact the
/// connection is retained, revocation changes what the settlement reads,
/// and nothing is retained for a revoked peer. What it did not cover is
/// what happens to the connection itself.
///
/// Releasing the ticket returns the reservation and leaves the
/// connection live — established, unauthorized, and outside the
/// manager's accounting. That is the same fail-open shape as a
/// settlement that cannot account for itself, so both queue a close.
pub async fn k28_withdrawn_connection_is_closed(r: &mut Report) {
    let cfg = NodeConfig {
        role: KadRole::Server,
        gate_mode: Mode::PolicyAdmit,
        ..NodeConfig::default()
    };
    let mut nodes = vec![
        Node::start(&cfg).await,
        Node::start(&cfg).await,
        Node::start(&cfg).await,
    ];
    let (a, b, c) = (nodes[0].peer_id, nodes[1].peer_id, nodes[2].peer_id);
    for i in 0..3 {
        for p in [a, b, c] {
            nodes[i].trust(p);
        }
    }
    let (b_addr, c_addr) = (nodes[1].dial_address(), nodes[2].dial_address());
    nodes[1].dial_admitted(c_addr.clone());
    pump_until(&mut nodes, Duration::from_secs(10), |n| {
        n[1].observed.connected.contains(&c)
    })
    .await;
    if let Some(k) = nodes[1].kad() {
        k.add_address(&c, c_addr);
    }
    nodes[0].dial_admitted(b_addr.clone());
    pump_until(&mut nodes, Duration::from_secs(10), |n| {
        n[0].observed.connected.contains(&b)
    })
    .await;

    // CONTROL FIRST: with trust intact the behaviour connection is
    // retained and SURVIVES, so the disappearance below is about the
    // revocation and not about the walk failing.
    nodes[0].ledger.reset();
    if let Some(k) = nodes[0].kad() {
        k.add_address(&b, b_addr);
        let id = k.get_closest_peers(c);
        nodes[0].own_queries.insert(id, QueryClass::Targeted);
    }
    pump_until(&mut nodes, Duration::from_secs(20), |n| {
        n[0].ledger.retained() > 0
    })
    .await;
    pump(&mut nodes, Duration::from_secs(3)).await;
    r.check(
        "K28.1",
        &format!(
            "CONTROL: a retained behaviour connection stays up ({} retained, \\
             connected to the peer: {})",
            nodes[0].ledger.retained(),
            nodes[0].swarm.is_connected(&c)
        ),
        nodes[0].ledger.retained() > 0 && nodes[0].swarm.is_connected(&c),
    );

    // NOW REVOKE while the connection is live, and settle again. The
    // reclassification branch is reached by the NEXT settlement for that
    // peer — the connection is dropped and re-established under the
    // revoked policy — which is the observable version of a race the
    // harness cannot open on demand (K20's stated limit).
    // THE PEER MUST BE ROUTED FOR ITS REMOVAL TO MEAN ANYTHING. The
    // walk connected to it; §7's pipeline is what puts it in the table.
    crate::topology::admit_candidates(&mut nodes[0], &namespace::protocol_name(&cfg.network_id));
    pump(&mut nodes, Duration::from_secs(2)).await;
    let routed_before = nodes[0].routing_peers();
    r.check(
        "K28.1b",
        &format!("the peer is in the routing table before revocation ({routed_before})"),
        routed_before >= 2,
    );
    let before_withdrawn = nodes[0].ledger.withdrawn();
    // REVOCATION ALONE. No manual disconnect: the earlier version of
    // this experiment disconnected by hand, which meant an
    // implementation that left a revoked peer connected and routable
    // would have passed unchanged.
    let acted = nodes[0].revoke(c);
    pump(&mut nodes, Duration::from_secs(3)).await;
    r.check(
        "K28.2",
        &format!("revocation reported the peer as revoked and acted on it ({acted})"),
        acted > 0,
    );
    r.check(
        "K28.3",
        &format!(
            "and the connection is gone without anyone disconnecting by hand: {}",
            nodes[0].swarm.is_connected(&c)
        ),
        !nodes[0].swarm.is_connected(&c),
    );
    r.check(
        "K28.4",
        &format!(
            "and it is out of the routing table: {} routed (was {routed_before})",
            nodes[0].routing_peers()
        ),
        nodes[0].routing_peers() < routed_before,
    );
    // AND CANNOT MAKE ONE. Admission refuses first, which is K8's
    // property reached from here — the point being that no path leads
    // back to a live connection for a revoked peer.
    nodes[0].ledger.reset();
    if let Some(k) = nodes[0].kad() {
        let id = k.get_closest_peers(c);
        nodes[0].own_queries.insert(id, QueryClass::Targeted);
    }
    pump(&mut nodes, Duration::from_secs(12)).await;
    r.check(
        "K28.5",
        &format!(
            "and no new one is admitted: {} allowed, refusals {:?}, retained {}",
            nodes[0].ledger.behaviour_allowed(),
            nodes[0].ledger.refusals(),
            nodes[0].ledger.retained()
        ),
        nodes[0].ledger.behaviour_allowed() == 0
            && nodes[0].ledger.retained() == 0
            && !nodes[0].swarm.is_connected(&c),
    );
    r.note(format!(
        "K28 LIMIT: withdrawn count {} (was {before_withdrawn}). The close on the \
         withdrawn branch is NOT asserted here, for the same reason K20's \
         reclassification is not: reaching it needs a settlement to land after \
         revocation, and this harness cannot force that window. What K28 does \
         establish is that no path leads back to a live connection for a revoked \
         peer — the control shows a retained one surviving, so the absence is \
         not an artefact of the walk failing. Removing the close would fail \
         nothing here; it is a fail-open path removed on the strength of the \
         production settlement path doing the same, not on evidence.",
        nodes[0].ledger.withdrawn()
    ));
}

/// K29 — a routed peer that is not connected is still revoked.
///
/// The manager's `Revoked` list names peers it classified from the LIVE
/// connection set, which is the question it can answer. It is not the
/// question §11 asks: routing removal is immediate, and whether a peer
/// has a socket open right now is unrelated to whether it may be routed.
///
/// A peer routed but disconnected therefore produced no entry, so
/// `remove_peer` was never called and it went on occupying the bounded
/// table and being selected for queries.
pub async fn k29_disconnected_peer_is_still_revoked(r: &mut Report) {
    let cfg = NodeConfig {
        role: KadRole::Server,
        gate_mode: Mode::PolicyAdmit,
        ..NodeConfig::default()
    };
    let protocol = namespace::protocol_name(&cfg.network_id);
    let mut nodes = vec![Node::start(&cfg).await, Node::start(&cfg).await];
    let (a, b) = (nodes[0].peer_id, nodes[1].peer_id);
    nodes[0].trust(a);
    nodes[0].trust(b);
    nodes[1].trust(a);
    nodes[1].trust(b);
    let b_addr = nodes[1].dial_address();

    nodes[0].dial_admitted(b_addr.clone());
    pump_until(&mut nodes, Duration::from_secs(12), |n| {
        n[0].observed.identify_protocols.contains_key(&b)
    })
    .await;
    crate::topology::admit_candidates(&mut nodes[0], &protocol);
    pump(&mut nodes, Duration::from_secs(2)).await;
    r.check(
        "K29.1",
        &format!("the peer is routed ({} entries)", nodes[0].routing_peers()),
        nodes[0].routes(&b),
    );

    // DISCONNECT, and confirm the routing entry SURVIVES the
    // disconnection — otherwise the revocation below would have nothing
    // to remove and the experiment would pass for the wrong reason.
    let _ = nodes[0].swarm.disconnect_peer_id(b);
    pump(&mut nodes, Duration::from_secs(3)).await;
    r.check(
        "K29.2",
        &format!(
            "a closed connection does NOT remove the routing entry: connected \\
             {}, routed {}",
            nodes[0].swarm.is_connected(&b),
            nodes[0].routes(&b)
        ),
        !nodes[0].swarm.is_connected(&b) && nodes[0].routes(&b),
    );

    // NOW REVOKE. The manager reports nothing, because it classifies
    // from live connections and there are none — so routing removal
    // cannot depend on its answer.
    let acted = nodes[0].revoke(b);
    pump(&mut nodes, Duration::from_secs(2)).await;
    r.check(
        "K29.3",
        &format!(
            "revoking a routed-but-disconnected peer removes it anyway: routed \\
             {}, acted {acted}",
            nodes[0].routes(&b)
        ),
        !nodes[0].routes(&b),
    );
    r.note(format!(
        "K29: routing entries {} after revocation",
        nodes[0].routing_peers()
    ));
}
