// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The trust overlay through the composition (ADR-0028 A 2026-10-07):
//! what `admin.trust.set` changed survives a restart of the runtime, is
//! applied before the first admission, and a set whose write fails
//! changes nothing and is audited `unwritten` -- each beside the control
//! that shows the mechanism, not the harness, made the difference.

#![allow(clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use interweave_local_client_api::{
    AdminBinding, AdminCapability, AdminPort, TrustAdminView, TrustSource, TrustedPeer,
};
use interweave_profile_config::ProfileConfig;
use interweave_profile_config::trust_overlay::TRUST_OVERLAY_FILE;
use interweave_profile_identity::ProfileIdentity;
use interweave_transport_api::{TransportError, TransportIdentity};
use interweave_transport_composition::{ComposedRuntime, CompositionError, CompositionOptions};

/// A profile allowing `trusted`, with one endpoint whose inbound subset
/// names `subset` -- a configured peer the overlay may come to revoke.
fn profile(trusted: &[&TransportIdentity], subset: &[&TransportIdentity]) -> ProfileConfig {
    let list = |peers: &[&TransportIdentity]| {
        peers
            .iter()
            .map(|p| format!("\"{}\"", p.as_str()))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let inbound = if subset.is_empty() {
        String::new()
    } else {
        format!(
            "\n      inbound:\n        static_subset: [{}]",
            list(subset)
        )
    };
    let doc = format!(
        "schema_version: 2
trust:
  policy: static-allowlist
  allowed_peers: [{}]
endpoints:
  entries:
    - id: human
      enabled: true
      advertise: false{inbound}
discovery:
  providers:
    - type: static-bootstrap
      enabled: true
      priority: 10
      config:
        peers: []
",
        list(trusted)
    );
    serde_norway::from_str(&doc).expect("the document parses")
}

fn id() -> (ProfileIdentity, TransportIdentity) {
    let identity = ProfileIdentity::generate();
    let peer = identity.transport_identity().expect("peer id");
    (identity, peer)
}

/// A private state directory and the overlay's path in it.
fn state() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("a temporary directory");
    chmod(dir.path(), 0o700);
    let path = dir.path().join(TRUST_OVERLAY_FILE);
    (dir, path)
}

fn chmod(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).expect("chmod");
}

fn options(overlay: Option<&Path>) -> CompositionOptions {
    CompositionOptions {
        trust_overlay_file: overlay.map(Path::to_path_buf),
        ..CompositionOptions::default()
    }
}

async fn trust(runtime: &ComposedRuntime) -> TrustAdminView {
    runtime
        .sessions()
        .admin([AdminCapability::Trust].into())
        .await
        .expect("a port")
        .trust()
        .await
        .expect("the policy")
}

async fn set(
    runtime: &ComposedRuntime,
    peer: &TransportIdentity,
    allowed: bool,
) -> Result<(), TransportError> {
    runtime
        .sessions()
        .admin([AdminCapability::Trust].into())
        .await
        .expect("a port")
        .set_trust(peer.clone(), allowed)
        .await
}

fn row(peer: &TransportIdentity, persisted: bool, source: TrustSource) -> TrustedPeer {
    TrustedPeer {
        peer: peer.clone(),
        persisted,
        source,
    }
}

/// A configured peer revoked and an unconfigured one allowed are still
/// so after the runtime restarts over the same overlay; the rows say
/// where each comes from and that they persist. The control is the same
/// sets on a runtime with no overlay: after its restart the
/// configuration alone is in force again, and no row says it persists.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_set_survives_a_restart_of_the_runtime() {
    let (identity, _) = id();
    let (_, kept) = id();
    let (_, revoked) = id();
    let (_, added) = id();
    let configured = profile(&[&kept, &revoked], &[]);
    for persisted in [true, false] {
        let (_dir, path) = state();
        let overlay = persisted.then_some(path.as_path());
        let runtime = ComposedRuntime::start(&identity, &configured, options(overlay))
            .await
            .expect("composes");
        set(&runtime, &revoked, false).await.expect("revoked");
        set(&runtime, &added, true).await.expect("allowed");
        runtime.stop().await.expect("stops");

        let runtime = ComposedRuntime::start(&identity, &configured, options(overlay))
            .await
            .expect("restarts");
        let mut rows = trust(&runtime).await.allowed;
        rows.sort();
        let mut expected = if persisted {
            vec![
                row(&kept, true, TrustSource::Configured),
                row(&added, true, TrustSource::Administered),
            ]
        } else {
            vec![
                row(&kept, false, TrustSource::Configured),
                row(&revoked, false, TrustSource::Configured),
            ]
        };
        expected.sort();
        assert_eq!(rows, expected, "persisted={persisted}");
        assert_eq!(path.exists(), persisted, "the file is the store");
        runtime.stop().await.expect("stops");
    }
}

/// The overlay is applied at start, before anything is admitted, and
/// against the configuration the profile validated with: a profile whose
/// endpoint subset names a configured peer the overlay revoked still
/// starts, and that peer is not allowed. The control is the same profile
/// with no overlay, which allows it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_overlay_is_in_force_from_the_start_against_the_configuration() {
    let (identity, _) = id();
    let (_, named) = id();
    let configured = profile(&[&named], &[&named]);
    let (_dir, path) = state();
    std::fs::write(
        &path,
        format!(r#"{{"added":[],"revoked":["{}"]}}"#, named.as_str()),
    )
    .expect("written");
    chmod(&path, 0o600);
    for overlay in [Some(path.as_path()), None] {
        let runtime = ComposedRuntime::start(&identity, &configured, options(overlay))
            .await
            .expect("starts with its endpoint naming a revoked peer");
        assert_eq!(
            trust(&runtime).await.peers().any(|p| p == &named),
            overlay.is_none(),
            "overlay={overlay:?}"
        );
        runtime.stop().await.expect("stops");
    }
}

/// A present overlay that cannot be trusted stops the start, never
/// skipped; the same contents, private, start.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_overlay_that_cannot_be_trusted_stops_the_start() {
    let (identity, _) = id();
    let (_, peer) = id();
    let configured = profile(&[&peer], &[]);
    let (_dir, path) = state();
    std::fs::write(&path, r#"{"added":[],"revoked":[]}"#).expect("written");
    for (mode, starts) in [(0o600, true), (0o644, false)] {
        chmod(&path, mode);
        match ComposedRuntime::start(&identity, &configured, options(Some(&path))).await {
            Ok(runtime) => {
                assert!(starts, "mode {mode:o} started");
                runtime.stop().await.expect("stops");
            }
            Err(e) => assert!(
                !starts && matches!(e, CompositionError::TrustOverlay(_)),
                "mode {mode:o}: {e}"
            ),
        }
    }
}

/// A set whose overlay write fails is answered `Internal` and changes
/// nothing -- the peer is not allowed, the next read says so -- and is
/// audited `unwritten`. The control is the same set once the state
/// directory is writable again: allowed, audited `changed`.
#[tokio::test(flavor = "current_thread")]
async fn a_set_whose_write_fails_changes_nothing_and_is_audited_unwritten() {
    // A current-thread runtime, so the driver's audit line is written on
    // this thread, under this test's subscriber.
    let lines = Arc::new(Mutex::new(Vec::<u8>::new()));
    let sink = Arc::clone(&lines);
    let subscriber = tracing_subscriber::fmt()
        .with_writer(move || Writer(Arc::clone(&sink)))
        .with_ansi(false)
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);

    let (identity, _) = id();
    let (_, stranger) = id();
    let configured = profile(&[], &[]);
    let (dir, path) = state();
    let runtime = ComposedRuntime::start(&identity, &configured, options(Some(&path)))
        .await
        .expect("composes");
    chmod(dir.path(), 0o500);
    let refused = set(&runtime, &stranger, true).await;
    chmod(dir.path(), 0o700);
    assert_eq!(refused, Err(TransportError::Internal));
    assert!(
        !trust(&runtime).await.peers().any(|p| p == &stranger),
        "nothing changed"
    );
    assert!(!path.exists(), "nothing written");

    set(&runtime, &stranger, true).await.expect("the control");
    assert!(trust(&runtime).await.peers().any(|p| p == &stranger));
    runtime.stop().await.expect("stops");

    let text = String::from_utf8(lines.lock().expect("lock").clone()).expect("utf-8");
    let audit: Vec<&str> = text
        .lines()
        .filter(|l| l.contains("admin.trust.set"))
        .collect();
    assert_eq!(audit.len(), 2, "{text}");
    assert!(audit[0].contains("outcome=\"unwritten\""), "{}", audit[0]);
    assert!(audit[1].contains("outcome=\"changed\""), "{}", audit[1]);
    for line in audit {
        assert!(
            !line.contains(&dir.path().display().to_string()),
            "no path in the audit line: {line}"
        );
    }
}

struct Writer(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Writer {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().expect("lock").extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
