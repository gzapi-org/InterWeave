// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The conformance suite's second runner (plan §16's exit gate, item 1):
//! the SAME generic functions as `in_process.rs`, against ipc-client ->
//! ipc-server -> the in-process binding, over real Unix sockets, with no
//! binding-specific branch. Each runtime of the shared fixture is served
//! on its own pair of sockets, and the suite holds an `IpcBinding` to each.
//!
//! The tests below are the in-process runner's generic-item tests with
//! only the fixture changed; `in_process.rs` keeps the tests that read the
//! in-process binding's own state, which a socket cannot show.

#![cfg(unix)]
#![allow(clippy::expect_used, clippy::panic)]

use std::time::Duration;

use interweave_ipc_client::{IpcBinding, SocketPaths as ClientPaths};
use interweave_ipc_server::{KeepalivePolicy, Limits, ServerConfig, SocketPaths, bind, serve};
use interweave_local_client_conformance_tests as suite;
use interweave_transport_api::{ChannelId, EndpointId, TransportIdentity};
use interweave_transport_composition::InProcessBinding;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

mod common;

use common::{Pair, agent, human};

/// One runtime's binding served on its own sockets.
struct Served {
    _root: tempfile::TempDir,
    stop: oneshot::Sender<()>,
    server: JoinHandle<()>,
    binding: IpcBinding,
}

impl Served {
    fn start(binding: InProcessBinding, peer: &TransportIdentity) -> Self {
        let root = tempfile::tempdir().expect("tempdir");
        let run_dir = root.path().join("interweave");
        let paths = SocketPaths {
            data: run_dir.join("data.sock"),
            admin: run_dir.join("admin.sock"),
            run_dir,
        };
        let listeners = bind(&paths).expect("binds");
        let config = ServerConfig {
            peer: peer.clone(),
            limits: Limits::default(),
            keepalive: KeepalivePolicy::default(),
            shutdown_grace: Duration::from_secs(1),
            command_deadline: Duration::from_secs(10),
            write_stall: interweave_ipc_server::WRITE_STALL,
        };
        let (stop, stopped) = oneshot::channel();
        let server = tokio::spawn(async move {
            let _ = serve(listeners, binding, config, async {
                let _ = stopped.await;
            })
            .await;
        });
        let client = IpcBinding::new(
            ClientPaths {
                data: paths.data.clone(),
                admin: paths.admin.clone(),
            },
            "conformance-admin",
        );
        Self {
            _root: root,
            stop,
            server,
            binding: client,
        }
    }

    async fn stop(self) {
        let _ = self.stop.send(());
        self.server.await.expect("the server stops");
    }
}

/// The shared pair, each runtime behind its own IPC server.
struct IpcPair {
    pair: Pair,
    a: Served,
    b: Served,
    a_peer: TransportIdentity,
    b_peer: TransportIdentity,
}

impl IpcPair {
    async fn start() -> Self {
        let pair = Pair::start().await;
        let (a, b) = pair.bindings();
        let a = Served::start(a, &pair.a_peer);
        let b = Served::start(b, &pair.b_peer);
        Self {
            a_peer: pair.a_peer.clone(),
            b_peer: pair.b_peer.clone(),
            pair,
            a,
            b,
        }
    }

    fn bindings(&self) -> (IpcBinding, IpcBinding) {
        (self.a.binding.clone(), self.b.binding.clone())
    }

    /// A's runtime restarted over its state, behind a new server.
    async fn restart_a(self) -> Self {
        let Self {
            pair,
            a,
            b,
            a_peer,
            b_peer,
        } = self;
        a.stop().await;
        let pair = pair.restart_a().await;
        let a = Served::start(pair.bindings().0, &a_peer);
        Self {
            pair,
            a,
            b,
            a_peer,
            b_peer,
        }
    }

    async fn stop(self) {
        self.a.stop().await;
        self.b.stop().await;
        self.pair.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn item_1_the_source_endpoint_is_the_senders_lease() {
    let pair = IpcPair::start().await;
    let (a, b) = pair.bindings();
    suite::the_source_endpoint_is_the_senders_lease(
        &a,
        &b,
        &pair.a_peer,
        &pair.b_peer,
        &agent(),
        &human(),
    )
    .await;
    pair.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_session_reports_its_profile_peer() {
    let pair = IpcPair::start().await;
    let (a, _) = pair.bindings();
    suite::a_session_reports_its_profile_peer(&a, &pair.a_peer, &pair.b_peer).await;
    pair.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn items_2_and_5_a_lease_is_exclusive_and_released_on_close() {
    let pair = IpcPair::start().await;
    let (a, _) = pair.bindings();
    suite::a_lease_is_exclusive_and_released_on_close(&a, &human()).await;
    pair.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn items_3_and_6_the_queue_is_bounded_and_acceptance_follows_admission() {
    let pair = IpcPair::start().await;
    let (a, b) = pair.bindings();
    suite::the_queue_is_bounded_and_acceptance_follows_admission(&a, &b, &pair.b_peer, &human())
        .await;
    pair.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_bounded_take_leaves_the_rest_queued_in_order() {
    let pair = IpcPair::start().await;
    let (a, b) = pair.bindings();
    suite::a_bounded_take_leaves_the_rest_queued_in_order(
        &a,
        &b,
        &pair.b_peer,
        &human(),
        &ChannelId::parse("general").expect("valid"),
    )
    .await;
    pair.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn item_4_local_refusals_map_exactly() {
    let pair = IpcPair::start().await;
    let (a, _) = pair.bindings();
    suite::local_refusals_map_exactly(
        &a,
        &pair.b_peer,
        &EndpointId::parse("nowhere").expect("valid"),
    )
    .await;
    pair.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn item_8_nothing_is_kept_for_an_unleased_endpoint() {
    let pair = IpcPair::start().await;
    let (a, b) = pair.bindings();
    suite::nothing_is_kept_for_an_unleased_endpoint(&a, &b, &pair.b_peer, &human()).await;
    pair.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn broadcast_reaches_joined_sessions_only() {
    let pair = IpcPair::start().await;
    let (a, b) = pair.bindings();
    suite::broadcast_reaches_joined_sessions_only(
        &a,
        &b,
        &pair.a_peer,
        &ChannelId::parse("general").expect("valid"),
    )
    .await;
    pair.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn item_7_administration_is_a_separate_authority() {
    let pair = IpcPair::start().await;
    let (a, _) = pair.bindings();
    suite::administration_is_a_separate_authority(&a, &human(), &pair.b_peer).await;
    pair.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn disabling_an_endpoint_revokes_and_never_rebinds() {
    let pair = IpcPair::start().await;
    let (a, _) = pair.bindings();
    suite::disabling_revokes_and_never_rebinds(&a, &human()).await;
    pair.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_admin_view_and_the_default_overlay() {
    let pair = IpcPair::start().await;
    let (a, _) = pair.bindings();
    suite::the_admin_view_and_the_default_overlay(&a, &pair.a_peer, &human(), &agent()).await;
    pair.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_directory_query_needs_its_capability() {
    let pair = IpcPair::start().await;
    let (a, b) = pair.bindings();
    suite::a_directory_query_needs_its_capability(&a, &b, &pair.b_peer, &agent()).await;
    pair.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn item_5_a_dropped_session_releases_its_lease() {
    let pair = IpcPair::start().await;
    let (a, _) = pair.bindings();
    suite::a_dropped_session_releases_its_lease(&a, &human()).await;
    pair.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn item_9_ready_resolves_on_what_waits_and_takes_nothing() {
    let pair = IpcPair::start().await;
    let (a, b) = pair.bindings();
    suite::ready_resolves_on_what_waits_and_takes_nothing(&a, &b, &pair.b_peer, &agent(), &human())
        .await;
    pair.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn item_10_the_runtimes_state_is_owed_once_at_open() {
    let pair = IpcPair::start().await;
    let (_, b) = pair.bindings();
    suite::the_runtimes_state_is_owed_once_at_open(&b).await;
    pair.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn peer_rows_answer_under_admin_status() {
    let pair = IpcPair::start().await;
    let (a, _) = pair.bindings();
    suite::peer_rows_answer_under_admin_status(&a, &pair.a_peer, &pair.b_peer).await;
    pair.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_revocation_survives_a_restart_of_the_runtime() {
    let pair = IpcPair::start().await;
    let b_peer = pair.b_peer.clone();
    suite::a_revocation_is_made_before_a_restart(&pair.bindings().0, &b_peer).await;
    let pair = pair.restart_a().await;
    suite::the_revocation_outlived_the_restart(&pair.bindings().0, &b_peer).await;
    pair.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn trust_administration_revokes_as_policy() {
    let pair = IpcPair::start().await;
    let (a, _) = pair.bindings();
    suite::trust_administration_revokes_as_policy(&a, &pair.a_peer, &pair.b_peer).await;
    pair.stop().await;
}
