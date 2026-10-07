// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! A trust set left ahead of the runtime, through the composition: a
//! binary of its own, because the warning it logs names no peer and the
//! log-reading tests in `trust_overlay.rs` read that warning from the one
//! log their binary shares (#215 re-review N1).

#![allow(clippy::expect_used, clippy::panic)]

use std::path::Path;

use interweave_transport_api::TransportError;
use interweave_transport_composition::ComposedRuntime;

mod common;

use common::{id, options, profile, set, state, trust};

/// A set left ahead of the runtime takes effect at the next start
/// whatever sets follow it (ADR-0028 A 2026-10-07, f290e85c; #215 review
/// F3): an ahead revocation survives a later allow of another peer, and
/// after an ahead allow, a revocation of that peer -- which the runtime,
/// never having allowed it, sees as no change -- reaches the file before
/// it is answered `ok`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_set_left_ahead_takes_effect_whatever_sets_follow() {
    use interweave_profile_config::trust_overlay::TrustOverlay;
    use interweave_transport_composition::OverlayFault::{AfterRename, BeforeRename};

    let (identity, _) = id();
    let (_, kept) = id();
    let (_, revoked) = id();
    let (_, other) = id();
    let configured = profile(&[&kept, &revoked], &[]);
    let peers = |path: &Path| {
        TrustOverlay::load(path, &configured.trust.allowed_peers)
            .expect("loads")
            .1
    };

    // An ahead revocation, then an allow of another peer.
    let (_dir, path) = state();
    let runtime = ComposedRuntime::start(&identity, &configured, options(Some(&path)))
        .await
        .expect("composes");
    runtime
        .fail_overlay_writes(vec![AfterRename, BeforeRename])
        .await
        .expect("queued");
    assert_eq!(
        set(&runtime, &revoked, false).await,
        Err(TransportError::Internal),
        "left ahead"
    );
    assert!(
        trust(&runtime).await.peers().any(|p| p == &revoked),
        "not published"
    );
    set(&runtime, &other, true).await.expect("a later set");
    runtime.stop().await.expect("stops");
    let next = peers(&path);
    assert!(!next.contains(&revoked), "the ahead revocation is in force");
    assert!(next.contains(&other) && next.contains(&kept), "{next:?}");

    // An ahead allow, then a revocation of the same peer.
    let (_dir, path) = state();
    let runtime = ComposedRuntime::start(&identity, &configured, options(Some(&path)))
        .await
        .expect("composes");
    runtime
        .fail_overlay_writes(vec![AfterRename, BeforeRename])
        .await
        .expect("queued");
    assert_eq!(
        set(&runtime, &other, true).await,
        Err(TransportError::Internal),
        "left ahead"
    );
    assert!(peers(&path).contains(&other), "ahead on disk");
    set(&runtime, &other, false).await.expect("answered ok");
    // And survives a later set on another peer (#215 re-review N2: the
    // driver kept the ahead lists, and the next set wrote them back).
    let (_, third) = id();
    set(&runtime, &third, true).await.expect("a later set");
    runtime.stop().await.expect("stops");
    let next = peers(&path);
    assert!(
        !next.contains(&other),
        "a revocation answered ok survives the restart: {next:?}"
    );
    assert!(next.contains(&third), "{next:?}");

    // An ahead revocation, the same peer allowed back -- answered ok, the
    // policy never having dropped it -- then a set on another peer.
    let (_dir, path) = state();
    let runtime = ComposedRuntime::start(&identity, &configured, options(Some(&path)))
        .await
        .expect("composes");
    runtime
        .fail_overlay_writes(vec![AfterRename, BeforeRename])
        .await
        .expect("queued");
    assert_eq!(
        set(&runtime, &revoked, false).await,
        Err(TransportError::Internal),
        "left ahead"
    );
    set(&runtime, &revoked, true)
        .await
        .expect("allowed back, answered ok");
    let (_, fourth) = id();
    set(&runtime, &fourth, true).await.expect("a later set");
    runtime.stop().await.expect("stops");
    let next = peers(&path);
    assert!(
        next.contains(&revoked),
        "an allow answered ok survives the restart: {next:?}"
    );
}

/// An overlay left ahead holds a peer the policy does not, so an allow
/// the policy's bound admits can be one past the overlay's: refused
/// `InvalidArgument` as the policy refuses at its own bound, nothing
/// written, and the next start starts with the ahead peer in force. The
/// same allow with room under both bounds is the control.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_allow_past_an_ahead_overlays_bound_is_refused() {
    use interweave_profile_config::trust_overlay::TrustOverlay;
    use interweave_transport_composition::OverlayFault::{AfterRename, BeforeRename};

    let max = interweave_trust_api::PeerTrustPolicy::MAX_ALLOWED_PEERS;
    let (identity, _) = id();
    // One short of the bound: room for one more in the policy.
    let others: Vec<_> = (1..max).map(|_| id().1).collect();
    let listed: Vec<_> = others.iter().collect();
    let configured = profile(&listed, &[]);
    let (_dir, path) = state();
    let runtime = ComposedRuntime::start(&identity, &configured, options(Some(&path)))
        .await
        .expect("composes");
    let (_, ahead) = id();
    runtime
        .fail_overlay_writes(vec![AfterRename, BeforeRename])
        .await
        .expect("queued");
    assert_eq!(
        set(&runtime, &ahead, true).await,
        Err(TransportError::Internal),
        "left ahead"
    );
    let (_, past) = id();
    assert_eq!(
        set(&runtime, &past, true).await,
        Err(TransportError::InvalidArgument),
        "the policy has room; the overlay on disk does not"
    );
    runtime.stop().await.expect("stops");

    let (_, allowed) = TrustOverlay::load(&path, &configured.trust.allowed_peers)
        .expect("the next start loads it");
    assert!(allowed.contains(&ahead) && !allowed.contains(&past));
    let runtime = ComposedRuntime::start(&identity, &configured, options(Some(&path)))
        .await
        .expect("starts");
    // The control: at the bound now, a revocation makes room again.
    set(&runtime, &ahead, false).await.expect("revoked");
    set(&runtime, &past, true).await.expect("room again");
    runtime.stop().await.expect("stops");
}
