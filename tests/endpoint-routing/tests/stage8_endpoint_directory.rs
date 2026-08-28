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
    querier
        .claim_endpoint("me", endpoint("human"), "in-process")
        .await
        .expect("command")
        .expect("free");
    let resolved = querier
        .send_direct(
            "me",
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
    querier
        .claim_endpoint("me", endpoint("human"), "in-process")
        .await
        .expect("command")
        .expect("free on the querier");
    let error = querier
        .send_direct(
            "me",
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
    let (querier, _responder, peer) =
        connected_for_directory(profile, &[("s", "human")]).await;

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
