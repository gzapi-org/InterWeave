// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The conformance suite against the embedded host (plan §20 step 1):
//! two `EmbeddedHost`s, each started from a provisioned profile under an
//! app data directory whose parent every walk to `/` refuses -- the
//! stand-in for Android's `system`-owned `/data/data` -- connected over
//! real sockets on the host's private address, each check run against
//! their `binding()`s. The same functions the other runners call, with no
//! branch of their own; the host drives them on its own executor, as the
//! Android Service's clients do.
//!
//! Beside them, gate (d)'s row (architect-cto, 2026-10-09): a private
//! directory under the platform's `files/` is refused by the host's
//! runtime root, where the walk alone accepts it.

#![allow(clippy::expect_used, clippy::panic)]
#![cfg(target_os = "linux")]

use std::collections::BTreeSet;
use std::future::Future;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use interweave_local_client_api::{AdminBinding, AdminCapability, AdminPort};
use interweave_local_client_conformance_tests as suite;
use interweave_profile_config::{
    PersistError, ProfilePaths, TrustBoundary, create_private_dir_within,
};
use interweave_profile_identity::{ProfileIdentity, RecoveryPhrase};
use interweave_transport_api::{ChannelId, EndpointId, TransportIdentity};
use interweave_transport_composition::InProcessBinding;
use interweave_transport_embedded::{EmbeddedHost, EmbeddedLaunch};

mod common;

use common::{agent, human};

const PROFILE: &str = "work";

/// The smallest delivery queue a profile may state
/// (`ipc.client_event_queue`), so the bound is reached quickly.
const QUEUE_BOUND: u32 = 16;

/// One app: its data directory under a refused `open`, its identity's
/// phrase to start again from, and the host while it runs.
struct Node {
    _root: tempfile::TempDir,
    app: PathBuf,
    phrase: RecoveryPhrase,
    peer: TransportIdentity,
    host: Option<EmbeddedHost>,
}

impl Node {
    fn new() -> Self {
        let root = tempfile::Builder::new()
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir()
            .expect("tempdir");
        let open = root.path().join("open");
        let app = open.join("app");
        for (dir, mode) in [(&open, 0o777), (&app, 0o700)] {
            std::fs::create_dir(dir).expect("mkdir");
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(mode)).expect("chmod");
        }
        let identity = ProfileIdentity::generate();
        Self {
            _root: root,
            app,
            phrase: identity.recovery_phrase().expect("a phrase"),
            peer: identity.transport_identity().expect("peer"),
            host: None,
        }
    }

    /// Write this app's `config.yaml`, trusting `trusted` and bootstrapping
    /// from `statics`, as the app provisions it before its first start.
    fn provision(&self, trusted: &TransportIdentity, statics: &[String]) {
        let ip = interweave_test_support::net::require_private_interface_v4();
        let peers: Vec<String> = statics.iter().map(|s| format!("\"{s}\"")).collect();
        let document = format!(
            "schema_version: 2
profile:
  name: {PROFILE}
runtime:
  deployment: embedded-android
ipc:
  enabled: false
  client_event_queue: {QUEUE_BOUND}
transport:
  listen:
    addresses: [\"/ip4/{ip}/tcp/0\"]
trust:
  policy: static-allowlist
  allowed_peers: [\"{}\"]
endpoints:
  default_direct_endpoint: human
  entries:
    - id: human
      enabled: true
      advertise: false
    - id: agent
      enabled: true
      advertise: true
channels:
  desired: [general]
discovery:
  providers:
    - type: static-bootstrap
      enabled: true
      priority: 10
      config:
        peers: [{}]
",
            trusted.as_str(),
            peers.join(", ")
        );
        let paths = ProfilePaths::resolve_embedded(
            PROFILE,
            TrustBoundary::new(&self.app).expect("a boundary"),
        )
        .expect("paths");
        create_private_dir_within(paths.config_dir(), paths.boundary()).expect("config dir");
        std::fs::write(paths.config_file(), document).expect("write");
        std::fs::set_permissions(paths.config_file(), std::fs::Permissions::from_mode(0o644))
            .expect("chmod");
    }

    fn start(&mut self) {
        let host = EmbeddedHost::start(EmbeddedLaunch {
            app_data_dir: self.app.clone(),
            profile: PROFILE.to_owned(),
            identity: ProfileIdentity::from_phrase(&self.phrase).expect("the identity"),
        })
        .expect("the host starts");
        self.host = Some(host);
    }

    fn host(&self) -> &EmbeddedHost {
        self.host.as_ref().expect("running")
    }

    fn stop(&mut self) {
        if let Some(host) = self.host.take() {
            host.stop(Duration::from_secs(1)).expect("the host stops");
        }
    }
}

/// Two hosts, A bootstrapping from B, both seeing the connection.
struct EmbeddedPair {
    a: Node,
    b: Node,
    a_peer: TransportIdentity,
    b_peer: TransportIdentity,
}

impl EmbeddedPair {
    fn start() -> Self {
        let (mut a, mut b) = (Node::new(), Node::new());
        b.provision(&a.peer, &[]);
        b.start();
        let b_addr = format!("{}/p2p/{}", b.host().listening()[0], b.peer.as_str());
        a.provision(&b.peer, &[b_addr]);
        a.start();
        let pair = Self {
            a_peer: a.peer.clone(),
            b_peer: b.peer.clone(),
            a,
            b,
        };
        Self::wait_connected(pair.a.host(), &pair.b_peer);
        Self::wait_connected(pair.b.host(), &pair.a_peer);
        pair
    }

    /// Until `host`'s admin view shows `peer` connected, within the
    /// suite's patience.
    fn wait_connected(host: &EmbeddedHost, peer: &TransportIdentity) {
        let capabilities: BTreeSet<AdminCapability> = [AdminCapability::Status].into();
        host.runtime().block_on(async {
            let port = host
                .binding()
                .admin(capabilities)
                .await
                .expect("an admin port");
            let deadline = tokio::time::Instant::now() + suite::PATIENCE;
            loop {
                let rows = port.peers().await.expect("peer rows");
                if rows.iter().any(|row| &row.peer == peer && row.connected) {
                    return;
                }
                assert!(
                    tokio::time::Instant::now() < deadline,
                    "{} not connected within {:?}: {rows:?}",
                    peer.as_str(),
                    suite::PATIENCE
                );
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        });
    }

    fn bindings(&self) -> (InProcessBinding, InProcessBinding) {
        (self.a.host().binding(), self.b.host().binding())
    }

    /// Run `check` on B's executor: A's may be stopped under it.
    fn run<F: Future>(&self, check: F) -> F::Output {
        self.b.host().runtime().block_on(check)
    }

    /// A stopped and started again over the same app directory: what its
    /// trust overlay kept is in force; nothing else of A is.
    fn restart_a(mut self) -> Self {
        self.a.stop();
        self.a.start();
        self
    }

    fn stop(mut self) {
        self.a.stop();
        self.b.stop();
    }
}

#[test]
fn item_1_the_source_endpoint_is_the_senders_lease() {
    let pair = EmbeddedPair::start();
    let (a, b) = pair.bindings();
    pair.run(suite::the_source_endpoint_is_the_senders_lease(
        &a,
        &b,
        &pair.a_peer,
        &pair.b_peer,
        &agent(),
        &human(),
    ));
    pair.stop();
}

#[test]
fn a_session_reports_its_profile_peer() {
    let pair = EmbeddedPair::start();
    let (a, _) = pair.bindings();
    pair.run(suite::a_session_reports_its_profile_peer(
        &a,
        &pair.a_peer,
        &pair.b_peer,
    ));
    pair.stop();
}

#[test]
fn items_2_and_5_a_lease_is_exclusive_and_released_on_close() {
    let pair = EmbeddedPair::start();
    let (a, _) = pair.bindings();
    pair.run(suite::a_lease_is_exclusive_and_released_on_close(
        &a,
        &human(),
    ));
    pair.stop();
}

#[test]
fn items_3_and_6_the_queue_is_bounded_and_acceptance_follows_admission() {
    let pair = EmbeddedPair::start();
    let (a, b) = pair.bindings();
    pair.run(
        suite::the_queue_is_bounded_and_acceptance_follows_admission(
            &a,
            &b,
            &pair.b_peer,
            &human(),
        ),
    );
    pair.stop();
}

#[test]
fn a_bounded_take_leaves_the_rest_queued_in_order() {
    let pair = EmbeddedPair::start();
    let (a, b) = pair.bindings();
    pair.run(suite::a_bounded_take_leaves_the_rest_queued_in_order(
        &a,
        &b,
        &pair.b_peer,
        &human(),
        &ChannelId::parse("general").expect("valid"),
    ));
    pair.stop();
}

#[test]
fn item_4_local_refusals_map_exactly() {
    let pair = EmbeddedPair::start();
    let (a, _) = pair.bindings();
    pair.run(suite::local_refusals_map_exactly(
        &a,
        &pair.b_peer,
        &EndpointId::parse("nowhere").expect("valid"),
    ));
    pair.stop();
}

#[test]
fn item_8_nothing_is_kept_for_an_unleased_endpoint() {
    let pair = EmbeddedPair::start();
    let (a, b) = pair.bindings();
    pair.run(suite::nothing_is_kept_for_an_unleased_endpoint(
        &a,
        &b,
        &pair.b_peer,
        &human(),
    ));
    pair.stop();
}

#[test]
fn broadcast_reaches_joined_sessions_only() {
    let pair = EmbeddedPair::start();
    let (a, b) = pair.bindings();
    pair.run(suite::broadcast_reaches_joined_sessions_only(
        &a,
        &b,
        &pair.a_peer,
        &ChannelId::parse("general").expect("valid"),
    ));
    pair.stop();
}

#[test]
fn item_7_administration_is_a_separate_authority() {
    let pair = EmbeddedPair::start();
    let (a, _) = pair.bindings();
    pair.run(suite::administration_is_a_separate_authority(
        &a,
        &human(),
        &pair.b_peer,
    ));
    pair.stop();
}

#[test]
fn disabling_an_endpoint_revokes_and_never_rebinds() {
    let pair = EmbeddedPair::start();
    let (a, _) = pair.bindings();
    pair.run(suite::disabling_revokes_and_never_rebinds(&a, &human()));
    pair.stop();
}

#[test]
fn the_admin_view_and_the_default_overlay() {
    let pair = EmbeddedPair::start();
    let (a, _) = pair.bindings();
    pair.run(suite::the_admin_view_and_the_default_overlay(
        &a,
        &pair.a_peer,
        &human(),
        &agent(),
    ));
    pair.stop();
}

#[test]
fn a_directory_query_needs_its_capability() {
    let pair = EmbeddedPair::start();
    let (a, b) = pair.bindings();
    pair.run(suite::a_directory_query_needs_its_capability(
        &a,
        &b,
        &pair.b_peer,
        &agent(),
    ));
    pair.stop();
}

#[test]
fn item_5_a_dropped_session_releases_its_lease() {
    let pair = EmbeddedPair::start();
    let (a, _) = pair.bindings();
    pair.run(suite::a_dropped_session_releases_its_lease(&a, &human()));
    pair.stop();
}

#[test]
fn item_9_ready_resolves_on_what_waits_and_takes_nothing() {
    let pair = EmbeddedPair::start();
    let (a, b) = pair.bindings();
    pair.run(suite::ready_resolves_on_what_waits_and_takes_nothing(
        &a,
        &b,
        &pair.b_peer,
        &agent(),
        &human(),
    ));
    pair.stop();
}

/// In process, as the in-process runner: the queues are the runtime's,
/// so a session ended by its host's stop has its end and nothing more.
#[test]
fn item_9_an_ended_session_answers_events_with_its_end() {
    let mut pair = EmbeddedPair::start();
    let (a, _) = pair.bindings();
    let session = pair.run(suite::a_session_to_end(&a, &human()));
    pair.a.stop();
    pair.run(suite::an_ended_session_answers_events_with_its_end(
        &session,
    ));
    pair.b.stop();
}

/// The case with something waiting at the end, degenerate in process
/// as the in-process runner's: the queues are the runtime's, and go
/// with its host's stop -- here the executor too, the session then
/// polled on the other host's.
#[test]
fn item_9_a_message_waiting_at_the_end_goes_with_the_host() {
    let mut pair = EmbeddedPair::start();
    let (a, b) = pair.bindings();
    let (session, _sender) = pair.run(suite::a_session_to_end_with_a_message_waiting(
        &b,
        &a,
        &pair.a_peer,
        &agent(),
        &human(),
    ));
    pair.a.stop();
    pair.run(suite::an_ended_session_answers_events_with_its_end(
        &session,
    ));
    pair.b.stop();
}

#[test]
fn item_10_a_route_begin_is_owed_the_peers_path() {
    let pair = EmbeddedPair::start();
    let (a, b) = pair.bindings();
    pair.run(suite::a_route_begin_is_owed_the_peers_path(
        &a,
        &b,
        &pair.a_peer,
        &pair.b_peer,
        &agent(),
        &human(),
        interweave_transport_api::PeerPath::Direct,
    ));
    pair.stop();
}

#[test]
fn item_10_the_runtimes_state_is_owed_once_at_open() {
    let pair = EmbeddedPair::start();
    let (_, b) = pair.bindings();
    pair.run(suite::the_runtimes_state_is_owed_once_at_open(&b));
    pair.stop();
}

#[test]
fn peer_rows_answer_under_admin_status() {
    let pair = EmbeddedPair::start();
    let (a, _) = pair.bindings();
    pair.run(suite::peer_rows_answer_under_admin_status(
        &a,
        &pair.a_peer,
        &pair.b_peer,
    ));
    pair.stop();
}

/// The revocation is kept in the trust overlay under the app's runtime
/// root, and the restarted host loads it from there.
#[test]
fn a_revocation_survives_a_restart_of_the_runtime() {
    let pair = EmbeddedPair::start();
    let b_peer = pair.b_peer.clone();
    pair.run(suite::a_revocation_is_made_before_a_restart(
        &pair.bindings().0,
        &b_peer,
    ));
    let pair = pair.restart_a();
    pair.run(suite::the_revocation_outlived_the_restart(
        &pair.bindings().0,
        &b_peer,
    ));
    pair.stop();
}

#[test]
fn trust_administration_revokes_as_policy() {
    let pair = EmbeddedPair::start();
    let (a, _) = pair.bindings();
    pair.run(suite::trust_administration_revokes_as_policy(
        &a,
        &pair.a_peer,
        &pair.b_peer,
    ));
    pair.stop();
}

/// Gate (d)'s row: the host's paths refuse a private directory under the
/// platform's `files/`, naming the runtime root, and create nothing; the
/// walk alone -- the same boundary without the root -- accepts it, which
/// is the control showing the root check, not the walk, refused it.
/// `files/` is `0711` here so the control holds on any host; on a device
/// it is `0771` with the app's own group, which ADR-0028's walk accepts
/// too (measured with the device evidence of step 1).
#[test]
fn gate_d_a_private_dir_under_files_is_refused_by_the_runtime_root() {
    let mut node = Node::new();
    let stranger = ProfileIdentity::generate()
        .transport_identity()
        .expect("peer");
    node.provision(&stranger, &[]);
    node.start();
    let files = node.app.join("files");
    std::fs::create_dir(&files).expect("mkdir");
    std::fs::set_permissions(&files, std::fs::Permissions::from_mode(0o711)).expect("chmod");
    let store = files.join("human");

    let walk_alone = TrustBoundary::new(&node.app).expect("a boundary");
    create_private_dir_within(&store, &walk_alone).expect("the control: the walk accepts it");
    std::fs::remove_dir(&store).expect("rmdir");

    let paths = node.host().paths();
    match create_private_dir_within(&store, paths.boundary()) {
        Err(PersistError::DirectoryNotPrivate { path, detail }) => {
            assert_eq!(path, store);
            let root: &Path = &node.app.join("interweave");
            assert!(
                detail.contains(&format!("outside the runtime root {}", root.display())),
                "{detail}"
            );
        }
        other => panic!("refused outside the runtime root: {other:?}"),
    }
    assert!(!store.exists(), "nothing created under files/");
    node.stop();
}
