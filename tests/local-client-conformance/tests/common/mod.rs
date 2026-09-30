// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The fixture both runners share: two runtimes composed from profiles,
//! connected over real sockets on the host's private address. The
//! in-process runner hands the suite their `sessions()` bindings; the IPC
//! runner serves those same bindings on the daemon's sockets and hands the
//! suite an `IpcBinding` to each, so the generic functions meet the same
//! runtimes either way.

#![allow(dead_code, clippy::expect_used, clippy::panic)]

use interweave_local_client_conformance_tests as suite;
use interweave_profile_config::ProfileConfig;
use interweave_profile_identity::ProfileIdentity;
use interweave_transport_api::{EndpointId, TransportEvent, TransportIdentity, TransportRuntime};
use interweave_transport_composition::{ComposedRuntime, CompositionOptions, InProcessBinding};

/// The endpoint queue bound both nodes run with: small, so the bound is
/// reachable without the ingress rate limits deciding first.
pub(crate) const QUEUE_BOUND: usize = 2;

pub(crate) fn profile(trusted: &TransportIdentity, statics: &[String]) -> ProfileConfig {
    let peers: Vec<String> = statics.iter().map(|s| format!("\"{s}\"")).collect();
    let doc = format!(
        "schema_version: 2
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
    serde_norway::from_str(&doc).expect("the document parses")
}

pub(crate) fn id() -> (ProfileIdentity, TransportIdentity) {
    let identity = ProfileIdentity::generate();
    let peer = identity.transport_identity().expect("peer id");
    (identity, peer)
}

pub(crate) async fn wait_connected(runtime: &mut ComposedRuntime, peer: &TransportIdentity) {
    let deadline = tokio::time::Instant::now() + suite::PATIENCE;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        match tokio::time::timeout(remaining, runtime.next_event()).await {
            Ok(Some(TransportEvent::PeerConnected { peer: got, .. })) if &got == peer => return,
            Ok(Some(_)) => {}
            Ok(None) => panic!("the runtime stopped"),
            Err(elapsed) => {
                // What the dial gate and discovery hold at the moment the
                // window closed: without it a missed connection is a
                // timeout and nothing else.
                let diagnostics = runtime.diagnostics().await;
                panic!(
                    "no PeerConnected from {} within {:?} ({elapsed}); diagnostics: {diagnostics:#?}",
                    peer.as_str(),
                    suite::PATIENCE
                )
            }
        }
    }
}

/// Two composed runtimes, A dialling B through its static entry, both
/// seeing the connection.
pub(crate) struct Pair {
    pub(crate) a: ComposedRuntime,
    pub(crate) b: ComposedRuntime,
    pub(crate) a_peer: TransportIdentity,
    pub(crate) b_peer: TransportIdentity,
}

impl Pair {
    pub(crate) async fn start() -> Self {
        let ip = interweave_test_support::net::require_private_interface_v4();
        let options = CompositionOptions {
            listen: vec![format!("/ip4/{ip}/tcp/0")],
            queue_bound: QUEUE_BOUND,
            ..CompositionOptions::default()
        };
        let (a_id, a_peer) = id();
        let (b_id, b_peer) = id();
        let mut b = ComposedRuntime::start(&b_id, &profile(&a_peer, &[]), options.clone())
            .await
            .expect("b composes");
        let b_addr = format!("{}/p2p/{}", b.listening()[0], b_peer.as_str());
        let mut a = ComposedRuntime::start(&a_id, &profile(&b_peer, &[b_addr]), options)
            .await
            .expect("a composes");
        wait_connected(&mut a, &b_peer).await;
        wait_connected(&mut b, &a_peer).await;
        Self {
            a,
            b,
            a_peer,
            b_peer,
        }
    }

    pub(crate) fn bindings(&self) -> (InProcessBinding, InProcessBinding) {
        (self.a.sessions(), self.b.sessions())
    }

    pub(crate) async fn stop(self) {
        self.a.shutdown().await.expect("a stops");
        self.b.shutdown().await.expect("b stops");
    }
}

pub(crate) fn human() -> EndpointId {
    EndpointId::parse("human").expect("valid")
}

pub(crate) fn agent() -> EndpointId {
    EndpointId::parse("agent").expect("valid")
}
