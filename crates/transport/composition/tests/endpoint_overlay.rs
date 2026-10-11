// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The endpoint overlay through the composition (ADR-0028 A 2026-10-11):
//! what `admin.endpoints.set_enabled` and `set_default` changed survives
//! a restart of the runtime -- plan §20 (e)'s evidence -- is on disk
//! before the set is answered, is put back when the set is not answered
//! `ok`, and is normalised and in force from the start; each beside the
//! control that shows the mechanism, not the harness, made the
//! difference.

#![allow(clippy::expect_used, clippy::panic)]

use std::path::Path;
use std::sync::{Arc, Mutex};

use interweave_local_client_api::{AdminBinding, AdminCapability, AdminPort, EndpointAdminView};
use interweave_profile_config::endpoint_overlay::{ENDPOINT_OVERLAY_FILE, EndpointOverlay};
use interweave_profile_config::{ProfileConfig, TrustBoundary};
use interweave_transport_api::{EndpointId, TransportError};
use interweave_transport_composition::{ComposedRuntime, CompositionError, CompositionOptions};

mod common;

use common::{chmod, id};

/// `human`, the configured default, and `agent`, both enabled.
fn profile() -> ProfileConfig {
    serde_norway::from_str(
        "schema_version: 2
trust:
  policy: static-allowlist
  allowed_peers: []
endpoints:
  default_direct_endpoint: human
  entries:
    - id: human
      enabled: true
      advertise: false
    - id: agent
      enabled: true
      advertise: false
discovery:
  providers:
    - type: static-bootstrap
      enabled: true
      priority: 10
      config:
        peers: []
",
    )
    .expect("the document parses")
}

fn ep(s: &str) -> EndpointId {
    EndpointId::parse(s).expect("an endpoint id")
}

/// A private state directory and the endpoint overlay's path in it.
fn state() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().expect("a temporary directory");
    chmod(dir.path(), 0o700);
    let path = dir.path().join(ENDPOINT_OVERLAY_FILE);
    (dir, path)
}

fn options(overlay: Option<&Path>) -> CompositionOptions {
    CompositionOptions {
        endpoint_overlay_file: overlay.map(Path::to_path_buf),
        ..CompositionOptions::default()
    }
}

async fn start(overlay: Option<&Path>) -> Result<ComposedRuntime, CompositionError> {
    let (identity, _) = id();
    ComposedRuntime::start(&identity, &profile(), options(overlay)).await
}

/// `(enabled, default, persisted)` of each endpoint, in id order.
async fn rows(runtime: &ComposedRuntime) -> Vec<(String, bool, bool, bool)> {
    let rows: Vec<EndpointAdminView> = runtime
        .sessions()
        .admin([AdminCapability::Endpoints].into())
        .await
        .expect("a port")
        .leases()
        .await
        .expect("the list");
    rows.into_iter()
        .map(|r| {
            (
                r.endpoint.as_str().to_owned(),
                r.enabled,
                r.default,
                r.persisted,
            )
        })
        .collect()
}

async fn set_enabled(
    runtime: &ComposedRuntime,
    endpoint: &str,
    enabled: bool,
) -> Result<(), TransportError> {
    runtime
        .sessions()
        .admin([AdminCapability::Endpoints].into())
        .await
        .expect("a port")
        .set_endpoint_enabled(ep(endpoint), enabled)
        .await
        .map(drop)
}

async fn set_default(
    runtime: &ComposedRuntime,
    endpoint: Option<&str>,
) -> Result<(), TransportError> {
    runtime
        .sessions()
        .admin([AdminCapability::Endpoints].into())
        .await
        .expect("a port")
        .set_default_endpoint(endpoint.map(ep))
        .await
}

/// What the next start would compose from the file at `path`, as rows
/// `(id, enabled, default)` in id order.
fn on_disk(path: &Path) -> Vec<(String, bool, bool)> {
    let config = profile().endpoints;
    let (overlay, _) =
        EndpointOverlay::load_within(path, &config, &TrustBoundary::root()).expect("loads");
    let effective = overlay.effective(&config);
    effective
        .enabled
        .iter()
        .map(|(id, on)| {
            (
                id.as_str().to_owned(),
                *on,
                effective.default.as_ref() == Some(id),
            )
        })
        .collect()
}

fn row(id: &str, enabled: bool, default: bool, persisted: bool) -> (String, bool, bool, bool) {
    (id.to_owned(), enabled, default, persisted)
}

/// THE GATE (e) EVIDENCE: a composed runtime has its default endpoint
/// disabled -- which clears the default -- and is dropped and rebuilt
/// over the same state directory; it lists the endpoint disabled, no
/// default, and every row `persisted`. A default set to the other
/// endpoint survives the same way. The control is the same sets on a
/// runtime with no overlay: after its restart the configuration alone is
/// in force again, and no row says it persists.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_disable_and_a_default_survive_a_restart_of_the_runtime() {
    for persisted in [true, false] {
        let (_dir, path) = state();
        let overlay = persisted.then_some(path.as_path());
        let runtime = start(overlay).await.expect("composes");
        set_enabled(&runtime, "human", false)
            .await
            .expect("disabled");
        assert_eq!(
            rows(&runtime).await,
            vec![
                row("agent", true, false, persisted),
                row("human", false, false, persisted),
            ],
            "persisted={persisted}: disabling the default cleared it"
        );
        drop(runtime);

        let runtime = start(overlay).await.expect("rebuilt");
        let expected = if persisted {
            vec![
                row("agent", true, false, true),
                row("human", false, false, true),
            ]
        } else {
            vec![
                row("agent", true, false, false),
                row("human", true, true, false),
            ]
        };
        assert_eq!(rows(&runtime).await, expected, "persisted={persisted}");

        set_default(&runtime, Some("agent")).await.expect("set");
        runtime.stop().await.expect("stops");
        let runtime = start(overlay).await.expect("restarts");
        let expected = if persisted {
            vec![
                row("agent", true, true, true),
                row("human", false, false, true),
            ]
        } else {
            vec![
                row("agent", true, false, false),
                row("human", true, true, false),
            ]
        };
        assert_eq!(rows(&runtime).await, expected, "persisted={persisted}");
        assert_eq!(path.exists(), persisted, "the file is the store");
        runtime.stop().await.expect("stops");
    }
}

/// A set answered `ok` is on disk when it is answered: the file the next
/// start would load already holds it, with the runtime still running.
/// The control is the file before the set: the configuration.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_set_is_on_disk_when_it_is_answered() {
    let (_dir, path) = state();
    let runtime = start(Some(&path)).await.expect("composes");
    assert!(!path.exists(), "the control: nothing written yet");
    set_enabled(&runtime, "agent", false)
        .await
        .expect("disabled");
    assert_eq!(
        on_disk(&path),
        vec![
            ("agent".to_owned(), false, false),
            ("human".to_owned(), true, true)
        ]
    );
    runtime.stop().await.expect("stops");
}

/// A set the overlay refuses -- an endpoint not configured, a default
/// naming a disabled one -- is answered as the runtime answers it and
/// writes nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_refused_set_writes_nothing() {
    let (_dir, path) = state();
    let runtime = start(Some(&path)).await.expect("composes");
    assert_eq!(
        set_enabled(&runtime, "nobody", false).await,
        Err(TransportError::EndpointUnknown)
    );
    assert_eq!(
        set_default(&runtime, Some("nobody")).await,
        Err(TransportError::EndpointUnknown)
    );
    assert!(!path.exists(), "nothing written");
    set_enabled(&runtime, "agent", false)
        .await
        .expect("disabled");
    let before = std::fs::read(&path).expect("the control: written");
    assert_eq!(
        set_default(&runtime, Some("agent")).await,
        Err(TransportError::EndpointDisabled)
    );
    assert_eq!(
        std::fs::read(&path).expect("read"),
        before,
        "nothing written"
    );
    runtime.stop().await.expect("stops");
}

/// A set whose overlay write fails is answered `Internal` and changes
/// nothing: the endpoint is still enabled, the next read says so, and no
/// file exists. The control is the same set once the state directory is
/// writable again.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_set_whose_write_fails_changes_nothing() {
    let (dir, path) = state();
    let runtime = start(Some(&path)).await.expect("composes");
    chmod(dir.path(), 0o500);
    let refused = set_enabled(&runtime, "agent", false).await;
    chmod(dir.path(), 0o700);
    assert_eq!(refused, Err(TransportError::Internal));
    assert_eq!(rows(&runtime).await[0], row("agent", true, false, true));
    assert!(!path.exists(), "nothing written");

    set_enabled(&runtime, "agent", false)
        .await
        .expect("the control");
    assert_eq!(rows(&runtime).await[0], row("agent", false, false, true));
    runtime.stop().await.expect("stops");
}

/// A disable whose write lands and then fails is put back: answered
/// `Internal`, nothing published, the file the next start loads holding
/// the previous state. A restore that itself lands and then fails has
/// put the previous bytes back too. A restore that fails before its
/// rename leaves the overlay AHEAD -- answered `Internal`, the warning
/// logged, the disable taking effect at the next start, and kept through
/// a later set on another endpoint. No fault is the control.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_set_whose_write_lands_and_then_fails_is_put_back_or_left_ahead() {
    use interweave_transport_composition::OverlayFault::{AfterRename, BeforeRename};

    let _ = logged();
    for (faults, answer, disabled_on_disk) in [
        (vec![AfterRename], Err(TransportError::Internal), false),
        (
            vec![AfterRename, AfterRename],
            Err(TransportError::Internal),
            false,
        ),
        (
            vec![AfterRename, BeforeRename],
            Err(TransportError::Internal),
            true,
        ),
        (Vec::new(), Ok(()), true),
    ] {
        let case = format!("{faults:?}");
        let ahead = faults == [AfterRename, BeforeRename];
        let (_dir, path) = state();
        let runtime = start(Some(&path)).await.expect("composes");
        let warned_before = ahead_warnings();
        runtime
            .fail_overlay_writes(faults.clone())
            .await
            .expect("queued");
        assert_eq!(
            set_enabled(&runtime, "agent", false).await,
            answer,
            "{case}"
        );
        assert_eq!(
            rows(&runtime).await[0].1,
            answer.is_err(),
            "{case}: published only when answered ok"
        );
        assert_eq!(
            ahead_warnings() > warned_before,
            ahead,
            "{case}: the ahead warning"
        );
        // A later set on the other endpoint keeps what the file holds.
        set_default(&runtime, None).await.expect("a later set");
        runtime.stop().await.expect("stops");
        let next = on_disk(&path);
        assert_eq!(
            next[0],
            ("agent".to_owned(), !disabled_on_disk, false),
            "{case}: what the next start loads"
        );
        assert_eq!(next[1], ("human".to_owned(), true, false), "{case}");
    }
}

/// A failed publish restores the previous overlay: answered
/// `BackendUnavailable`, nothing published, the next start loads the
/// previous state. The same set once the publish works is the control.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_publish_restores_the_previous_overlay() {
    let (_dir, path) = state();
    let runtime = start(Some(&path)).await.expect("composes");
    set_enabled(&runtime, "agent", false)
        .await
        .expect("a first set");
    runtime.fail_publishes(1).await.expect("queued");
    assert_eq!(
        set_default(&runtime, None).await,
        Err(TransportError::BackendUnavailable)
    );
    assert!(rows(&runtime).await[1].2, "human is still the default");
    assert!(
        on_disk(&path)[1].2,
        "restored: human is the default on disk"
    );

    set_default(&runtime, None).await.expect("the control");
    assert!(!on_disk(&path)[1].2);
    runtime.stop().await.expect("stops");
}

/// A failed publish whose restore then fails before its rename leaves
/// the overlay AHEAD: answered `Internal`, never the publish's own
/// `BackendUnavailable`, since the set does take effect at the next
/// start -- the file says so and the runtime does not.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_publish_whose_restore_fails_is_left_ahead() {
    use interweave_transport_composition::OverlayFault::{BeforeRename, Clean};

    let (_dir, path) = state();
    let runtime = start(Some(&path)).await.expect("composes");
    runtime.fail_publishes(1).await.expect("queued");
    // The set's own write is clean; the restore's meets the fault.
    runtime
        .fail_overlay_writes(vec![Clean, BeforeRename])
        .await
        .expect("queued");
    assert_eq!(
        set_enabled(&runtime, "agent", false).await,
        Err(TransportError::Internal)
    );
    assert!(
        rows(&runtime).await[0].1,
        "not published: agent still enabled"
    );
    assert!(!on_disk(&path)[0].1, "ahead: disabled on disk");
    runtime.stop().await.expect("stops");
}

/// A present overlay that cannot be trusted stops the start, never
/// skipped; the same contents, private, start with them in force.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_overlay_that_cannot_be_trusted_stops_the_start() {
    let (_dir, path) = state();
    std::fs::write(&path, r#"{"enabled":{"agent":false}}"#).expect("written");
    for (mode, starts) in [(0o600, true), (0o644, false)] {
        chmod(&path, mode);
        match start(Some(&path)).await {
            Ok(runtime) => {
                assert!(starts, "mode {mode:o} started");
                assert_eq!(rows(&runtime).await[0], row("agent", false, false, true));
                runtime.stop().await.expect("stops");
            }
            Err(e) => assert!(
                !starts && matches!(e, CompositionError::EndpointOverlay(_)),
                "mode {mode:o}: {e}"
            ),
        }
    }
}

/// An overlay that disables the configured default is normalised at
/// start: the default cleared, the file rewritten to say so, a warning
/// naming the entry, and the runtime started with the endpoint disabled
/// and no default. The control is the same profile with no overlay.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_overlay_is_normalised_and_in_force_from_the_start() {
    let _ = logged();
    let (_dir, path) = state();
    std::fs::write(&path, r#"{"enabled":{"human":false}}"#).expect("written");
    chmod(&path, 0o600);
    let runtime = start(Some(&path)).await.expect("starts");
    assert_eq!(
        rows(&runtime).await,
        vec![
            row("agent", true, false, true),
            row("human", false, false, true)
        ]
    );
    assert_eq!(
        std::fs::read_to_string(&path).expect("read"),
        "{\n  \"enabled\": {\n    \"human\": false\n  },\n  \"default\": null\n}"
    );
    assert!(
        logged().contains("the configured default human is disabled"),
        "the warning names the entry"
    );
    runtime.stop().await.expect("stops");

    let runtime = start(None).await.expect("the control");
    assert_eq!(rows(&runtime).await[1], row("human", true, true, false));
    runtime.stop().await.expect("stops");
}

/// `human` and `agent` both advertised, `agent` disabled, at most one
/// advertised: `config.yaml` validates, since only enabled endpoints
/// count toward `directory.max_advertised`.
fn one_advertised() -> ProfileConfig {
    serde_norway::from_str(
        "schema_version: 2
trust:
  policy: static-allowlist
  allowed_peers: []
endpoints:
  directory:
    max_advertised: 1
  entries:
    - id: human
      enabled: true
      advertise: true
    - id: agent
      enabled: false
      advertise: true
discovery:
  providers:
    - type: static-bootstrap
      enabled: true
      priority: 10
      config:
        peers: []
",
    )
    .expect("the document parses")
}

/// A SET THE NEXT START WOULD REFUSE IS REFUSED NOW (#261 review F1):
/// enabling `agent` past `directory.max_advertised` is answered
/// `InvalidArgument`, nothing written, the runtime unchanged -- answered
/// `ok`, the composed profile would fail validation at the next start.
/// The control: with `human` disabled first, the same enable is room
/// under the bound and survives a restart.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_set_the_next_start_would_refuse_is_refused_and_writes_nothing() {
    let (identity, _) = id();
    let (_dir, path) = state();
    let profile = one_advertised();
    let start = || ComposedRuntime::start(&identity, &profile, options(Some(&path)));
    let runtime = start().await.expect("composes");
    assert_eq!(
        set_enabled(&runtime, "agent", true).await,
        Err(TransportError::InvalidArgument)
    );
    assert!(!path.exists(), "nothing written");
    assert_eq!(rows(&runtime).await[0], row("agent", false, false, true));

    set_enabled(&runtime, "human", false)
        .await
        .expect("disabled");
    set_enabled(&runtime, "agent", true)
        .await
        .expect("the control: room under the bound");
    runtime.stop().await.expect("stops");
    let runtime = start().await.expect("the next start composes");
    assert_eq!(
        rows(&runtime).await,
        vec![
            row("agent", true, false, true),
            row("human", false, false, true)
        ]
    );
    runtime.stop().await.expect("stops");
}

/// A file no set could have written -- hand-edited, or left by a
/// `config.yaml` edit lowering the bound -- that composes into a profile
/// failing validation stops the start naming the overlay, never as a
/// `config.yaml` that validates alone. The control: the same
/// `config.yaml` with no overlay starts.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_overlay_the_composed_profile_refuses_stops_the_start_by_name() {
    let (identity, _) = id();
    let (_dir, path) = state();
    std::fs::write(&path, r#"{"enabled":{"agent":true}}"#).expect("written");
    chmod(&path, 0o600);
    match ComposedRuntime::start(&identity, &one_advertised(), options(Some(&path))).await {
        Err(CompositionError::EndpointOverlayConflicts(errors)) => {
            assert!(!errors.is_empty());
        }
        Err(other) => panic!("refused naming the overlay: {other}"),
        Ok(_) => panic!("refused"),
    }
    ComposedRuntime::start(&identity, &one_advertised(), options(None))
        .await
        .expect("the control")
        .stop()
        .await
        .expect("stops");
}

/// How many ahead warnings the runtimes in this binary have logged.
fn ahead_warnings() -> usize {
    logged()
        .matches("the endpoint overlay is ahead of the runtime")
        .count()
}

/// Everything the runtimes in this binary have logged so far, through
/// ONE subscriber installed globally once (`trust_overlay.rs` says why).
fn logged() -> String {
    static LOG: std::sync::OnceLock<Arc<Mutex<Vec<u8>>>> = std::sync::OnceLock::new();
    let log = LOG.get_or_init(|| {
        let log = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&log);
        let subscriber = tracing_subscriber::fmt()
            .with_writer(move || Writer(Arc::clone(&sink)))
            .with_ansi(false)
            .finish();
        tracing::subscriber::set_global_default(subscriber).expect("the one subscriber");
        log
    });
    String::from_utf8(log.lock().expect("lock").clone()).expect("utf-8")
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
