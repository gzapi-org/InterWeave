// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! `/interweave/endpoints/1.0.0` over the wire: a trusted peer learns
//! which routes another advertises, and an untrusted one learns nothing.
//!
//! Real sockets, because the claim is about what one node discloses to
//! another across a connection. The pure filtering, validation and
//! budget rules are unit-tested in `transport-runtime`; here the whole
//! path runs, and each negative assertion carries a positive control in
//! the same test (the Stage 7 lesson: two tests once passed while
//! proving nothing).
#![allow(clippy::expect_used, clippy::panic)]

mod support;

use interweave_transport_api::TransportError;
use support::{
    advertised, advertised_to, connected_for_directory, endpoint, profile_directory, who,
};

fn names(result: &interweave_transport_libp2p::runtime::DirectoryResult) -> Vec<&str> {
    result
        .endpoints
        .iter()
        .map(interweave_transport_api::EndpointId::as_str)
        .collect()
}

/// testing.md 16: the directory lists ONLY endpoints that are advertised,
/// leased, and admissible for the querier — four endpoints, one listed.
///
/// Mutations (each a conjunct of `advertised_for`, all unit-tested in the
/// registry): unadvertised, unleased, or policy-excluded would each
/// wrongly appear.
#[tokio::test]
async fn a_trusted_peer_learns_only_active_advertised_admissible_routes() {
    // A third peer the responder trusts, so `for-someone-else` can narrow
    // to it — and thereby exclude the querier, which is the point.
    let (_stranger_id, stranger) = who();
    let profile = support::profile_trusting(
        &[&stranger],
        vec![
            advertised("listed"),                         // advertised + leased => shown
            advertised("unleased"),                       // advertised, never claimed => not
            support::entry("private"),                    // leased, not advertised => not
            advertised_to("for-someone-else", &stranger), // admits stranger, not querier
        ],
        Some("listed"),
    );
    // Claim everything except `unleased`.
    let (querier, _responder, peer) = connected_for_directory(
        profile,
        &[
            ("s-listed", "listed"),
            ("s-private", "private"),
            ("s-else", "for-someone-else"),
        ],
    )
    .await;

    let result = querier
        .query_endpoints(peer)
        .await
        .expect("the command reaches the task")
        .expect("a trusted peer receives a directory");
    assert_eq!(
        names(&result),
        ["listed"],
        "only the one that meets every clause"
    );
    assert!(!result.cached, "the first query crossed the wire");
}

/// testing.md 21 / 142: a node refuses to disclose — or even ask for — a
/// directory across a trust boundary. The reachable guard is local: a
/// querier asked about a peer it does NOT trust returns UnauthorizedPeer
/// with no packet, so there is no oracle and no wire exchange.
///
/// The responder-side refusal (an untrusted peer that reached the
/// responder) is not reachable end to end at this stage: an
/// infrastructure-only or untrusted peer cannot hold an inbound
/// connection, so the socket closes before a query — the connection layer
/// doing the directory's exclusion for it (ADR-0036). That is stated in
/// the Met. block as a limit, not a gap.
#[tokio::test]
async fn querying_a_peer_you_do_not_trust_is_refused_locally() {
    use interweave_transport_libp2p::runtime::{SubstrateConfig, SwarmRuntime};

    let (node_id, _node_peer) = who();
    let (_stranger_id, stranger) = who();
    let node = SwarmRuntime::start(&node_id, SubstrateConfig::default(), support::trusting(&[]))
        .expect("the node starts");

    let error = node
        .query_endpoints(stranger)
        .await
        .expect("the command reaches the task")
        .expect_err("a query about an untrusted peer is refused before any packet");
    assert_eq!(error, TransportError::UnauthorizedPeer);
}

/// testing.md 21 positive control, as its own test: a data-plane trusted
/// peer on an otherwise identical responder DOES get the list.
#[tokio::test]
async fn the_same_directory_answers_a_trusted_querier() {
    let profile = profile_directory(vec![advertised("human")], Some("human"), true);
    let (querier, _responder, peer) = connected_for_directory(profile, &[("s", "human")]).await;

    let result = querier
        .query_endpoints(peer)
        .await
        .expect("command")
        .expect("a trusted querier receives the directory");
    assert_eq!(names(&result), ["human"]);
}

/// testing.md 17: a disabled directory refuses queries, and explicit
/// endpoint sends keep working.
#[tokio::test]
async fn a_disabled_directory_does_not_break_explicit_send() {
    let profile = profile_directory(vec![advertised("human")], Some("human"), false);
    let (querier, _responder, peer) = connected_for_directory(profile, &[("s", "human")]).await;

    let error = querier
        .query_endpoints(peer.clone())
        .await
        .expect("command")
        .expect_err("the directory is off");
    assert_eq!(
        error,
        TransportError::RemoteEndpointUnavailable,
        "unavailable, coarsely — not a statement about any endpoint"
    );

    // The querier holds no lease here, so it cannot send; but the
    // responder still ROUTES to `human`, proving the directory being off
    // did not disable endpoint delivery. Claim on the querier and send.
    let me = querier
        .claim_endpoint("me", endpoint("human"), "in-process")
        .await
        .expect("command")
        .expect("free");
    let resolved = querier
        .send_direct(
            &me,
            peer,
            support::frame("human", Some("human"), b"still routes", 1),
        )
        .await
        .expect("command")
        .expect("an explicit send works with the directory disabled");
    assert_eq!(resolved, endpoint("human"));
}

/// testing.md 18: a cached entry survives the endpoint's shutdown, and an
/// explicit send then returns the ordinary no_route — the cache is
/// advisory and does not promise the route still works.
#[tokio::test]
async fn a_stale_cache_entry_then_release_yields_no_route() {
    let profile = profile_directory(vec![advertised("human")], Some("human"), true);
    let (querier, responder, peer) = connected_for_directory(profile, &[("s", "human")]).await;

    let first = querier
        .query_endpoints(peer.clone())
        .await
        .expect("command")
        .expect("directory");
    assert_eq!(names(&first), ["human"]);

    // The responder's session goes; the route is gone.
    let released = responder.release_session("s").await.expect("command");
    assert_eq!(released, vec![endpoint("human")]);

    // The cache still answers — advisory, from local receipt, unaware the
    // route died.
    let cached = querier
        .query_endpoints(peer.clone())
        .await
        .expect("command")
        .expect("the cache still holds it");
    assert!(cached.cached, "answered from cache, no wire");
    assert_eq!(names(&cached), ["human"]);

    // But an explicit send is the ordinary no_route: the querier claims a
    // source and sends to the now-unleased endpoint.
    let me = querier
        .claim_endpoint("me", endpoint("human"), "in-process")
        .await
        .expect("command")
        .expect("free on the querier");
    let error = querier
        .send_direct(
            &me,
            peer,
            support::frame("human", Some("human"), b"gone", 2),
        )
        .await
        .expect("command")
        .expect_err("the route the cache still lists is gone");
    assert_eq!(error, TransportError::RemoteEndpointUnavailable);
}

/// testing.md 26: trust revocation removes directory access at once, not
/// when a cached entry happens to expire. The querier warms its cache,
/// then drops the responder from trust; the next query is refused before
/// the still-fresh cache is consulted.
///
/// Mutation: serve the cache before the trust check (the previous order)
/// and the revoked peer keeps reading its cached routes.
#[tokio::test]
async fn revoking_trust_removes_cached_directory_access_at_once() {
    let profile = profile_directory(vec![advertised("human")], Some("human"), true);
    let (querier, _responder, peer) = connected_for_directory(profile, &[("s", "human")]).await;

    // Warm the cache: this entry stays fresh for the default 60s.
    let warm = querier
        .query_endpoints(peer.clone())
        .await
        .expect("command")
        .expect("the first query succeeds and caches");
    assert_eq!(names(&warm), ["human"]);

    // Drop the responder from trust. The connection close is asynchronous,
    // but the local class check does not wait for it.
    querier
        .set_trust(support::trusting(&[]))
        .await
        .expect("the trust update reaches the task");

    let error = querier
        .query_endpoints(peer)
        .await
        .expect("the command reaches the task")
        .expect_err("a revoked peer cannot read its cached directory");
    assert_eq!(error, TransportError::UnauthorizedPeer);
}

/// A peer revoked while a query is in flight never has its directory
/// surfaced to the caller. Two layers enforce this and either is enough:
/// revoking trust closes the connection, so the exchange usually fails
/// before a response arrives; and if a response DID arrive first — the
/// narrow window this test cannot force deterministically — the response
/// arm re-reads the class and refuses it before caching.
///
/// The deterministic, meaningful assertion is the property itself: after
/// revocation the in-flight query resolves to an error, never an Ok
/// carrying the revoked peer's endpoints. The responder holds its answer
/// until the test has revoked trust (awaiting the command's reply), so
/// the answer can only reach the querier after the revocation.
#[tokio::test]
async fn a_revoked_peers_directory_is_not_surfaced_to_an_in_flight_query() {
    use interweave_transport_api::EndpointDirectoryV1;
    use interweave_transport_libp2p::endpoints_codec::{
        DirectoryResponse, ENDPOINTS_PROTOCOL, EndpointsCodec,
    };
    use interweave_transport_libp2p::runtime::{SubstrateConfig, SwarmRuntime};
    use libp2p::futures::StreamExt;
    use libp2p::request_response;
    use libp2p::swarm::SwarmEvent as RawEvent;
    use std::time::Duration;

    let responder_keys = libp2p::identity::Keypair::generate_ed25519();
    let responder_peer = interweave_transport_api::TransportIdentity::parse(
        responder_keys.public().to_peer_id().to_string(),
    )
    .expect("a valid peer id");

    let mut responder = libp2p::SwarmBuilder::with_existing_identity(responder_keys)
        .with_tokio()
        .with_tcp(
            libp2p::tcp::Config::default(),
            libp2p::noise::Config::new,
            libp2p::yamux::Config::default,
        )
        .expect("the same transport stack")
        .with_behaviour(|_| {
            request_response::Behaviour::<EndpointsCodec>::new(
                [(ENDPOINTS_PROTOCOL, request_response::ProtocolSupport::Full)],
                request_response::Config::default(),
            )
        })
        .expect("behaviour")
        .build();
    responder
        .listen_on("/ip4/127.0.0.1/tcp/0".parse().expect("loopback"))
        .expect("listens");
    let address = loop {
        if let RawEvent::NewListenAddr { address, .. } = responder.select_next_some().await {
            break address;
        }
    };

    // Responder task: on the request, tell the test and hold the channel;
    // on the trigger, answer with a one-entry directory.
    let (got_request_tx, got_request_rx) = tokio::sync::oneshot::channel();
    let (answer_now_tx, mut answer_now_rx) = tokio::sync::mpsc::channel::<()>(1);
    tokio::spawn(async move {
        let mut held = None;
        let mut told = Some(got_request_tx);
        loop {
            tokio::select! {
                event = responder.select_next_some() => {
                    if let RawEvent::Behaviour(request_response::Event::Message {
                        message: request_response::Message::Request { channel, .. },
                        ..
                    }) = event
                    {
                        held = Some(channel);
                        if let Some(tx) = told.take() {
                            let _ = tx.send(());
                        }
                    }
                }
                _ = answer_now_rx.recv() => {
                    if let Some(channel) = held.take() {
                        let _ = responder.behaviour_mut().send_response(
                            channel,
                            DirectoryResponse::Directory(EndpointDirectoryV1 {
                                generated_at_ms: 1,
                                ttl_ms: 60_000,
                                endpoints: vec![endpoint("human")],
                            }),
                        );
                    }
                }
            }
        }
    });

    let (querier_id, _querier_peer) = who();
    let mut querier = SwarmRuntime::start(
        &querier_id,
        SubstrateConfig::default(),
        support::trusting(&[&responder_peer]),
    )
    .expect("the querier starts");
    querier
        .dial(responder_peer.clone(), address)
        .await
        .expect("command")
        .expect("admitted");

    // The connection must be up before the query, or begin_query refuses
    // it as unreachable and the responder never sees a request.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        assert!(
            tokio::time::Instant::now() < deadline,
            "no connection within 20s"
        );
        match tokio::time::timeout(Duration::from_secs(20), querier.next_event()).await {
            Ok(Some(interweave_transport_libp2p::runtime::SwarmEvent::Connected { .. })) => break,
            Ok(Some(_)) => {}
            Ok(None) => panic!("the querier stopped before connecting"),
            Err(_) => panic!("no connection within 20s"),
        }
    }
    let querier = querier;

    // Dispatch the query in a task; it blocks on the reply.
    let q = {
        let querier = &querier;
        let peer = responder_peer.clone();
        async move { querier.query_endpoints(peer).await }
    };
    let peer_for_revoke = responder_peer.clone();
    let ((), result) = tokio::join!(
        async {
            // The request reached the responder; now revoke and let it answer.
            tokio::time::timeout(Duration::from_secs(20), got_request_rx)
                .await
                .expect("the responder received the query within 20s")
                .expect("the responder task is alive");
            let _ = peer_for_revoke;
            querier
                .set_trust(support::trusting(&[]))
                .await
                .expect("the revocation is processed");
            answer_now_tx.send(()).await.expect("trigger the answer");
        },
        tokio::time::timeout(Duration::from_secs(20), q),
    );

    let outcome = result
        .expect("the query settled rather than hanging")
        .expect("the command reaches the task");
    // Never Ok: whichever layer wins — the connection close or the
    // response-arm recheck — the revoked peer's endpoints are not
    // delivered. Both refusals are correct.
    assert!(
        matches!(
            outcome,
            Err(TransportError::UnauthorizedPeer | TransportError::PeerUnreachable)
        ),
        "a revoked peer's directory must not be surfaced, got {outcome:?}"
    );
}

/// The largest legal directory crosses the wire: 32 advertised, leased,
/// admissible endpoints at the 64-byte label ceiling. The codec's frozen
/// bytes are unit-tested; this proves the whole path carries them.
#[tokio::test]
async fn the_largest_legal_directory_crosses_the_wire() {
    let long = |i: usize| format!("{}{i:02}", "e".repeat(62));
    let entries: Vec<_> = (0..32).map(|i| advertised(&long(i))).collect();
    let sessions: Vec<(String, String)> = (0..32).map(|i| (format!("s{i}"), long(i))).collect();
    let session_refs: Vec<(&str, &str)> = sessions
        .iter()
        .map(|(s, n)| (s.as_str(), n.as_str()))
        .collect();
    let profile = profile_directory(entries, None, true);

    let (querier, _responder, peer) = connected_for_directory(profile, &session_refs).await;

    let result = querier
        .query_endpoints(peer)
        .await
        .expect("command")
        .expect("32 entries at the ceiling round-trip");
    assert_eq!(result.endpoints.len(), 32);
    assert!(
        result.endpoints.iter().all(|e| e.as_str().len() == 64),
        "every label at the 64-byte ceiling"
    );
    assert!(result.endpoints.windows(2).all(|w| w[0] <= w[1]), "sorted");
}

/// testing.md 146: a refused query reveals no endpoint list — the Err
/// arm is what a refusal produces, never an Ok carrying a list.
///
/// The per-peer rate bound itself (the 13th query in a minute) is unit-
/// tested in `transport-runtime::directory`
/// (`the_thirteenth_query_in_a_minute_is_refused`): end to end it is not
/// reachable, because the requester cache answers every query after the
/// first from one responder, so a 13-query burst never reaches the
/// responder's bucket. That is a stated limit, not a gap — forcing it
/// would mean disabling the cache, a configuration no caller uses.
#[tokio::test]
async fn a_refusal_carries_no_endpoint_list() {
    let profile = profile_directory(vec![advertised("human")], Some("human"), false);
    let (querier, _responder, peer) = connected_for_directory(profile, &[("s", "human")]).await;

    let error = querier
        .query_endpoints(peer)
        .await
        .expect("command")
        .expect_err("the directory is disabled");
    // The error carries no endpoints — the type cannot; the point is that
    // the Err arm is what a refusal produces, never an Ok with a list.
    assert_eq!(error, TransportError::RemoteEndpointUnavailable);
}

/// A draining node refuses a new directory query, the same as a direct
/// send. `ShuttingDown` is local; nothing crosses the wire, and even a
/// cached entry is not served — the node has said it is going away.
///
/// Mutation: drop the is_draining check in begin_query and the drained
/// node answers the query, serving cache or dialing after shutdown began.
#[tokio::test]
async fn a_draining_node_refuses_a_new_directory_query() {
    let profile = profile_directory(vec![advertised("human")], Some("human"), true);
    let (querier, _responder, peer) = connected_for_directory(profile, &[("s", "human")]).await;

    // Warm the cache with one good query, so the refusal below cannot be
    // mistaken for a cache miss.
    querier
        .query_endpoints(peer.clone())
        .await
        .expect("command")
        .expect("the first query succeeds");

    querier.drain().await.expect("the drain reaches the task");

    let error = querier
        .query_endpoints(peer)
        .await
        .expect("the command reaches the task")
        .expect_err("a draining node starts no new work and serves no cache");
    assert_eq!(error, TransportError::ShuttingDown);
}

/// generated_at_ms is a wall-clock epoch-ms timestamp, not monotonic
/// elapsed since the responder started — otherwise a consumer rendering
/// it would show a 1970 date after every restart.
///
/// Mutation: set generated_at_ms from now_ms (monotonic) and the value
/// falls below the epoch threshold, since a freshly started runtime's
/// elapsed time is a few milliseconds.
#[tokio::test]
async fn generated_at_ms_is_wall_clock_not_monotonic() {
    let profile = profile_directory(vec![advertised("human")], Some("human"), true);
    let (querier, _responder, peer) = connected_for_directory(profile, &[("s", "human")]).await;

    let result = querier
        .query_endpoints(peer)
        .await
        .expect("command")
        .expect("directory");

    // 2020-01-01 in epoch-ms. A wall clock is well past it; monotonic
    // elapsed since a runtime that started moments ago is a handful of ms.
    const YEAR_2020_MS: u64 = 1_577_836_800_000;
    assert!(
        result.generated_at_ms > YEAR_2020_MS,
        "generated_at_ms {} is not a wall-clock timestamp",
        result.generated_at_ms
    );
    // Freshness is a DURATION, not that wall-clock timestamp: a fresh
    // result advertised at 60s and clamped is good for at most the
    // five-minute ceiling, nowhere near an epoch value.
    assert!(
        result.fresh_for_ms > 0 && result.fresh_for_ms <= 300_000,
        "fresh_for_ms {} is a remaining duration, not a timestamp",
        result.fresh_for_ms
    );
}

/// A disabled directory still charges a trusted peer's rate: refusals are
/// not free. The 13th query in a minute is refused Overloaded (the rate),
/// not Unavailable (the disabled state), proving every trust-admitted
/// query — refusal included — consumed the 12/minute budget.
///
/// Deterministic: refusals are not cached, so each query crosses the wire
/// and the responder charges the rate each time.
///
/// Mutation: charge the rate only for a served directory (the old order)
/// and every disabled query returns Unavailable, so the 13th never
/// becomes Overloaded.
#[tokio::test]
async fn a_disabled_directory_still_charges_the_query_rate() {
    let profile = profile_directory(vec![advertised("human")], Some("human"), false);
    let (querier, _responder, peer) = connected_for_directory(profile, &[("s", "human")]).await;

    // The default per-peer budget is 12/minute. The first twelve queries
    // to the disabled directory are Unavailable; each still pays the rate.
    for i in 0..12 {
        let error = querier
            .query_endpoints(peer.clone())
            .await
            .expect("command")
            .expect_err("the directory is disabled");
        assert_eq!(
            error,
            TransportError::RemoteEndpointUnavailable,
            "query {i} is unavailable but charged"
        );
    }
    // The thirteenth is over the rate: Overloaded, not Unavailable. Had a
    // disabled refusal skipped the charge, this would still be Unavailable.
    let error = querier
        .query_endpoints(peer)
        .await
        .expect("command")
        .expect_err("the rate is spent");
    assert_eq!(
        error,
        TransportError::Overloaded,
        "the 13th query is refused by the rate the disabled ones charged"
    );
}

/// A responder that disables its OWN cache still advertises a cacheable
/// directory: the advertised TTL is a separate concept from the local
/// cache setting, so tuning one does not silently zero the other.
///
/// Mutation: advertise `local_cache_ttl_ms` instead of the dedicated
/// advertised TTL, and a responder whose cache is 0 hands the querier a
/// zero-freshness result.
#[tokio::test]
async fn the_advertised_ttl_is_independent_of_the_local_cache_setting() {
    use interweave_transport_libp2p::runtime::{SubstrateConfig, SwarmRuntime};

    // The responder caches nothing of its own (cache TTL 0) but must still
    // advertise the protocol default so peers may cache its directory.
    let (querier_id, querier_peer) = who();
    let (responder_id, _) = who();
    let responder_config = SubstrateConfig {
        directory_cache_ttl_ms: 0,
        ..SubstrateConfig::default()
    };
    let mut responder = SwarmRuntime::start(
        &responder_id,
        responder_config,
        support::trusting(&[&querier_peer]),
    )
    .expect("responder starts");
    let responder_peer = responder.local_peer().clone();
    let responder_peer_for_query = responder_peer.clone();
    let querier = SwarmRuntime::start(
        &querier_id,
        SubstrateConfig::default(),
        support::trusting(&[&responder_peer]),
    )
    .expect("querier starts");

    let profile = profile_directory(vec![advertised("human")], Some("human"), true);
    responder
        .configure_direct(
            interweave_transport_libp2p::runtime::DirectEndpoints::from_profile(&profile, 8)
                .expect("valid"),
        )
        .await
        .expect("installs");
    responder
        .claim_endpoint("s", endpoint("human"), "in-process")
        .await
        .expect("command")
        .expect("free");
    let address = responder
        .listen("/ip4/127.0.0.1/tcp/0".parse().expect("loopback"))
        .await
        .expect("listens");
    querier
        .dial(responder_peer, address)
        .await
        .expect("command")
        .expect("admitted");
    support::wait_connected(&mut responder).await;

    let result = querier
        .query_endpoints(responder_peer_for_query)
        .await
        .expect("command")
        .expect("directory");
    assert_eq!(names(&result), ["human"]);
    assert!(
        result.fresh_for_ms > 0,
        "the responder's zero cache setting must not zero what it advertises"
    );
}

/// A profile's configured query rate is honoured, not the built-in
/// default. A responder set to 1 query/minute refuses the SECOND query
/// from one peer — where the default 12 would admit it. The querier
/// caches nothing (cache TTL 0) so both queries reach the responder.
///
/// Mutation: build the responder budget with `with_defaults` regardless
/// of the profile, and the second query is admitted.
#[tokio::test]
async fn the_configured_query_rate_is_honoured() {
    use interweave_profile_config::DirectoryConfig;
    use interweave_transport_libp2p::runtime::{DirectEndpoints, SubstrateConfig, SwarmRuntime};

    let (querier_id, querier_peer) = who();
    let (responder_id, _) = who();
    let mut responder = SwarmRuntime::start(
        &responder_id,
        SubstrateConfig::default(),
        support::trusting(&[&querier_peer]),
    )
    .expect("responder starts");
    let responder_peer = responder.local_peer().clone();
    // The querier never caches, so every query crosses the wire.
    let querier = SwarmRuntime::start(
        &querier_id,
        SubstrateConfig {
            directory_cache_ttl_ms: 0,
            ..SubstrateConfig::default()
        },
        support::trusting(&[&responder_peer]),
    )
    .expect("querier starts");

    // A responder that admits ONE directory query per minute per peer.
    let mut profile = profile_directory(vec![advertised("human")], Some("human"), true);
    profile.endpoints.directory = DirectoryConfig {
        max_queries_per_minute_per_peer: 1,
        ..profile.endpoints.directory
    };
    responder
        .configure_direct(DirectEndpoints::from_profile(&profile, 8).expect("valid"))
        .await
        .expect("installs");
    responder
        .claim_endpoint("s", endpoint("human"), "in-process")
        .await
        .expect("command")
        .expect("free");
    let address = responder
        .listen("/ip4/127.0.0.1/tcp/0".parse().expect("loopback"))
        .await
        .expect("listens");
    querier
        .dial(responder_peer.clone(), address)
        .await
        .expect("command")
        .expect("admitted");
    support::wait_connected(&mut responder).await;

    // First query: admitted.
    let first = querier
        .query_endpoints(responder_peer.clone())
        .await
        .expect("command")
        .expect("the first query is within the rate");
    assert_eq!(names(&first), ["human"]);

    // Second: over the configured rate of one. The default of twelve would
    // have admitted it.
    let error = querier
        .query_endpoints(responder_peer)
        .await
        .expect("command")
        .expect_err("the second query exceeds the configured rate of one");
    assert_eq!(error, TransportError::Overloaded);
}

/// A profile's directory.cache_ttl reaches the requester cache: a querier
/// whose profile sets a 10s cache clamps a result to 10s, where the 60s
/// default would leave it at 60s.
///
/// Mutation: skip set_cache_ttl at configure, and the querier caches for
/// the default 60s regardless of its profile.
#[tokio::test]
async fn the_profile_cache_ttl_reaches_the_requester_cache() {
    use interweave_profile_config::DirectoryConfig;
    use interweave_transport_libp2p::runtime::{DirectEndpoints, SubstrateConfig, SwarmRuntime};

    let (querier_id, querier_peer) = who();
    let (responder_id, _) = who();
    let mut responder = SwarmRuntime::start(
        &responder_id,
        SubstrateConfig::default(),
        support::trusting(&[&querier_peer]),
    )
    .expect("responder starts");
    let responder_peer = responder.local_peer().clone();
    let querier = SwarmRuntime::start(
        &querier_id,
        SubstrateConfig::default(),
        support::trusting(&[&responder_peer]),
    )
    .expect("querier starts");

    // The querier's own profile caches directory results for 10 seconds.
    let mut querier_profile = profile_directory(vec![advertised("human")], Some("human"), true);
    querier_profile.endpoints.directory = DirectoryConfig {
        cache_ttl_ms: 10_000,
        ..querier_profile.endpoints.directory
    };
    querier
        .configure_direct(DirectEndpoints::from_profile(&querier_profile, 8).expect("valid"))
        .await
        .expect("querier installs");

    // The responder advertises the 60s default.
    let responder_profile = profile_directory(vec![advertised("human")], Some("human"), true);
    responder
        .configure_direct(DirectEndpoints::from_profile(&responder_profile, 8).expect("valid"))
        .await
        .expect("responder installs");
    responder
        .claim_endpoint("s", endpoint("human"), "in-process")
        .await
        .expect("command")
        .expect("free");
    let address = responder
        .listen("/ip4/127.0.0.1/tcp/0".parse().expect("loopback"))
        .await
        .expect("listens");
    querier
        .dial(responder_peer.clone(), address)
        .await
        .expect("command")
        .expect("admitted");
    support::wait_connected(&mut responder).await;

    let result = querier
        .query_endpoints(responder_peer)
        .await
        .expect("command")
        .expect("directory");
    assert!(
        result.fresh_for_ms > 0 && result.fresh_for_ms <= 10_000,
        "the profile's 10s cache_ttl clamps the result, got {}ms",
        result.fresh_for_ms
    );
}

/// The exit gate: route discovery works without entering broadcast,
/// peer discovery, or Kademlia state. The querier configures no channels
/// and adds no addresses beyond the dial, and the query still succeeds.
#[tokio::test]
async fn route_discovery_touches_no_broadcast_or_discovery_state() {
    let profile = profile_directory(vec![advertised("human")], Some("human"), true);
    let (querier, _responder, peer) = connected_for_directory(profile, &[("s", "human")]).await;

    // No configure_broadcast, no join, no add_address: a bare query.
    let result = querier
        .query_endpoints(peer)
        .await
        .expect("command")
        .expect("discovery of a route needs none of the other subsystems");
    assert_eq!(names(&result), ["human"]);
}

/// The directory originates no dial: a query to a TRUSTED but unconnected
/// peer fails PeerUnreachable rather than dialing to find out. The
/// query_endpoints path refuses to call send_request on an unconnected
/// peer, the same double guard direct sends have.
#[tokio::test]
async fn the_directory_never_originates_a_dial() {
    use interweave_transport_libp2p::runtime::{SubstrateConfig, SwarmRuntime};

    let (node_id, _node_peer) = who();
    let (_absent_id, absent_peer) = who();
    // A lone node that TRUSTS `absent_peer` but has never connected to it.
    let node = SwarmRuntime::start(
        &node_id,
        SubstrateConfig::default(),
        support::trusting(&[&absent_peer]),
    )
    .expect("the node starts");

    let error = node
        .query_endpoints(absent_peer)
        .await
        .expect("the command reaches the task")
        .expect_err("trusted but unconnected: the directory does not dial");
    assert_eq!(
        error,
        TransportError::PeerUnreachable,
        "no dial is originated to satisfy a directory query"
    );
}
