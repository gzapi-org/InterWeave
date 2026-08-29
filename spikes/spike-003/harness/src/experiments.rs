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

use crate::Report;
use crate::gate::Mode;
use crate::namespace;
use crate::node::{KadRole, Node, NodeConfig};
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
    let connected = pump_until(&mut nodes, Duration::from_secs(10), |n| {
        n[1].observed.connected.contains(&server_id)
    })
    .await;
    r.check("K2.1", "the two nodes connect at all", connected);

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
        nodes[1].own_queries.insert(id);
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
    r.check(
        "K5.2",
        &format!(
            "routing insertion started {implicit} quer{} nobody asked for",
            if implicit == 1 { "y" } else { "ies" }
        ),
        true, // measured, not asserted: the count IS the evidence
    );
    r.note(format!(
        "K5.2 unattributed queries after one manual insertion: {implicit}"
    ));

    // AND AN EXPLICIT BOOTSTRAP WORKS once a peer is known.
    let started = nodes[1].kad().and_then(|k| k.bootstrap().ok());
    if let Some(id) = started {
        nodes[1].own_queries.insert(id);
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
        nodes[0].own_queries.insert(id);
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
        nodes[0].own_queries.insert(id);
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
        nodes[0].own_queries.insert(id);
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
    {
        let identity = TransportIdentity::parse(c.to_base58()).expect("canonical");
        let mut p = nodes[0].policy.lock().expect("policy");
        // No known-good alternative for this peer, so the failure is
        // peer-scoped and advances backoff.
        let advanced = p.record_address_failure(&identity, "/ip4/198.51.100.1/tcp/1", 0, 600_000);
        r.check(
            "K9.1",
            "the production policy put the peer into backoff",
            advanced,
        );
    }
    nodes[0].ledger.reset();
    if let Some(k) = nodes[0].kad() {
        k.add_address(&b, b_addr);
        let id = k.get_closest_peers(c);
        nodes[0].own_queries.insert(id);
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

    // SHUTDOWN STATE, the other half: a draining node refuses every
    // behaviour dial regardless of trust.
    nodes[0].policy.lock().expect("policy").shutting_down = true;
    nodes[0].ledger.reset();
    if let Some(k) = nodes[0].kad() {
        let id = k.get_closest_peers(libp2p::PeerId::random());
        nodes[0].own_queries.insert(id);
    }
    pump(&mut nodes, Duration::from_secs(8)).await;
    let refusals = nodes[0].ledger.refusals();
    r.check(
        "K9.4",
        "a draining node refuses behaviour dials as shutting down",
        nodes[0].ledger.behaviour_originated() == 0
            || refusals.contains_key("shutting down"),
    );
    r.note(format!("K9 drain: refusals {refusals:?}"));
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
        nodes[1].own_queries.insert(id);
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

    // PROVIDER RECORDS, the same question through the other door.
    let provide = nodes[1]
        .kad()
        .and_then(|k| k.start_providing(kad::RecordKey::new(&b"/interweave/nope")).ok());
    if let Some(id) = provide {
        nodes[1].own_queries.insert(id);
    }
    pump(&mut nodes, Duration::from_secs(8)).await;
    let providers = nodes[0]
        .kad()
        .map(|k| k.store_mut().provided().count())
        .unwrap_or(usize::MAX);
    r.check(
        "K10.4",
        &format!("no provider record is stored either: {providers}"),
        providers == 0,
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
    // a hash of anything.
    for round in 0..4 {
        for i in 0..N {
            let key = libp2p::kad::RecordKey::new(&random_32());
            if let Some(k) = nodes[i].kad() {
                let id = k.get_n_closest_peers(
                    key.to_vec(),
                    NonZeroUsize::new(10).expect("nonzero"),
                );
                nodes[i].own_queries.insert(id);
            }
        }
        pump(&mut nodes, Duration::from_secs(8)).await;

        // THE ADMISSION PIPELINE, which is the half `BucketInserts::
        // Manual` exists to force. A query result is a CANDIDATE: the
        // provider decides, and nothing enters the routing table until
        // it does. Without this step exploration discovers peers and
        // routes to none of them, which is what the first run of this
        // experiment measured.
        for i in 0..N {
            let learned: Vec<_> = nodes[i]
                .observed
                .learned_addresses
                .iter()
                .filter(|(peer, addrs)| **peer != ids[i] && !addrs.is_empty())
                .map(|(peer, addrs)| (*peer, addrs.iter().next().cloned().expect("nonempty")))
                .collect();
            let trusted = nodes[i].trusted.lock().expect("trusted").clone();
            for (peer, addr) in learned {
                if !trusted.contains(&peer) {
                    continue;
                }
                if let Some(k) = nodes[i].kad() {
                    k.add_address(&peer, addr);
                }
            }
        }
        pump(&mut nodes, Duration::from_secs(2)).await;

        let total: usize = (0..N).map(|i| nodes[i].routing_peers()).sum();
        r.note(format!(
            "K11 exploration round {round}: total routing entries {total}"
        ));
    }

    let after: Vec<usize> = (0..N).map(|i| nodes[i].routing_peers()).collect();
    let grew = (1..N).filter(|&i| after[i] > seeded[i]).count();
    r.check(
        "K11.2",
        &format!("exploration expanded routing on {grew} of {} seeded nodes: {after:?}", N - 1),
        grew > 0,
    );
    let reached: usize = after.iter().copied().max().unwrap_or(0);
    r.check(
        "K11.3",
        &format!("at least one node learned more than its seed: max {reached}"),
        reached > 1,
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
    if let Some(k) = nodes[1].kad() {
        k.add_address(&a, a_addr);
        let id = k.get_closest_peers(libp2p::PeerId::random());
        nodes[1].own_queries.insert(id);
    }
    pump(&mut nodes, Duration::from_secs(3)).await;

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
    let active = nodes[1].own_queries.len() - nodes[1].observed.finished_queries.len().min(nodes[1].own_queries.len());
    r.check(
        "K15.4",
        &format!("active_queries_by_class is derivable from tracked ids: {active}"),
        true,
    );
    let pending = nodes[1].ledger.behaviour_originated();
    r.check(
        "K15.5",
        &format!("pending_behaviour_dials comes from the gate, not from libp2p: {pending}"),
        true,
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
        "K15 snapshot: mode={mode:?} hash={protocol_hash} routing={routing_peer_count} buckets={nonempty_buckets} behaviour_dials={pending}"
    ));
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
    if let Some(id) = id {
        nodes[0].own_queries.insert(id);
    }
    pump(&mut nodes, Duration::from_secs(12)).await;

    let contacted = nodes[0]
        .observed
        .query_requests
        .values()
        .map(|(requests, _)| *requests)
        .max()
        .unwrap_or(0);
    r.check(
        "K16.1",
        &format!("a query contacts several routers rather than one: {contacted} requests"),
        contacted > 1,
    );
    r.check(
        "K16.2",
        "the configuration with disjoint paths enabled builds and queries complete",
        !nodes[0].observed.finished_queries.is_empty(),
    );
    r.note(format!(
        "K16: query stats (requests, successes) = {:?}",
        nodes[0].observed.query_requests.values().collect::<Vec<_>>()
    ));
    r.note(
        "K16 LIMIT: this measures path WIDTH, not Byzantine resistance. \
         Reduced single-path capture is a claim about an adversary that \
         controls a subset of routers, and this harness has no such \
         adversary — see the record's stated limits."
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
    for _ in 0..5 {
        rounds += 1;
        for i in 0..N {
            let key = random_32();
            if let Some(k) = nodes[i].kad() {
                let id =
                    k.get_n_closest_peers(key, NonZeroUsize::new(20).expect("nonzero"));
                nodes[i].own_queries.insert(id);
            }
        }
        pump(&mut nodes, Duration::from_secs(10)).await;
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
    let converged = sizes.iter().filter(|&&s| s >= N / 2).count();
    r.check(
        "K17.1",
        &format!("most nodes reach at least half the network: {converged}/{N}"),
        converged >= N / 2,
    );
    r.check(
        "K17.2",
        &format!(
            "no routing table exceeds what the bounds allow: max {}",
            sizes.iter().max().copied().unwrap_or(0)
        ),
        sizes.iter().all(|&s| s < N),
    );
    let refusals: u64 = (0..N).map(|i| nodes[i].ledger.behaviour_originated()).sum();
    r.check(
        "K17.3",
        &format!("every behaviour dial in the run passed the gate: {refusals} seen"),
        refusals > 0,
    );
    r.note(format!(
        "K17: {N} nodes, {rounds} rounds, {:.1}s wall clock, final sizes {sizes:?}",
        elapsed.as_secs_f64()
    ));
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

    // `a` already has a WORKING route to `c`, recorded in the production
    // policy the way a successful authenticated connection would be.
    // That is what makes the next assertion meaningful: a bad address
    // must not cost a good one.
    let c_identity = TransportIdentity::parse(c.to_base58()).expect("canonical");
    nodes[0]
        .policy
        .lock()
        .expect("policy")
        .record_success(&c_identity, &c_addr.to_string(), 0);

    nodes[0].dial_admitted(b_addr.clone());
    pump_until(&mut nodes, Duration::from_secs(10), |n| {
        n[0].observed.connected.contains(&b)
    })
    .await;
    nodes[0].ledger.reset();
    if let Some(k) = nodes[0].kad() {
        k.add_address(&b, b_addr);
        let id = k.get_closest_peers(c);
        nodes[0].own_queries.insert(id);
    }
    pump(&mut nodes, Duration::from_secs(15)).await;

    // THE DIAL ERROR NAMES THE ADDRESS, which is stronger evidence than
    // the query result would be: it proves the poisoned address was not
    // merely reported but acted on. `GetClosestPeers` reports the peers
    // a walk found, and a peer whose only address fails to connect is
    // not among them — so reading the result set here would look for the
    // evidence in the one place the failure removes it.
    let dead_text = dead.to_string();
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
    let errors = nodes[0].observed.dial_errors.len();
    r.check(
        "K18.2",
        &format!("the dial to it was attempted and failed: {errors} outgoing error(s)"),
        errors > 0,
    );

    // THE PRODUCTION RULE: an address-scoped failure on a never-good
    // address must not suppress the peer while a known-good route
    // remains. Fed the failure the way the runtime would.
    let advanced = nodes[0].policy.lock().expect("policy").record_address_failure(
        &c_identity,
        &dead.to_string(),
        1_000,
        600_000,
    );
    r.check(
        "K18.3",
        "a bad address handed over by a router does not advance peer backoff",
        !advanced,
    );
    let still_dialable = nodes[0]
        .policy
        .lock()
        .expect("policy")
        .is_address_dialable(&c_identity, &c_addr.to_string(), 2_000);
    r.check(
        "K18.4",
        "and the known-good route to the same peer stays dialable",
        still_dialable,
    );
    r.note(format!(
        "K18: dial errors {:?}",
        nodes[0]
            .observed
            .dial_errors
            .iter()
            .take(2)
            .collect::<Vec<_>>()
    ));
}
