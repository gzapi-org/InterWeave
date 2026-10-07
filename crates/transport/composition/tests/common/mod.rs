// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! What the trust overlay tests share: a profile, identities, a private
//! state directory, the options naming its overlay, and the admin port's
//! read and set.

#![allow(dead_code, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};

use interweave_local_client_api::{
    AdminBinding, AdminCapability, AdminPort, TrustAdminView, TrustSource, TrustedPeer,
};
use interweave_profile_config::ProfileConfig;
use interweave_profile_config::trust_overlay::TRUST_OVERLAY_FILE;
use interweave_profile_identity::ProfileIdentity;
use interweave_transport_api::{TransportError, TransportIdentity};
use interweave_transport_composition::{ComposedRuntime, CompositionOptions};

/// A profile allowing `trusted`, with one endpoint whose inbound subset
/// names `subset` -- a configured peer the overlay may come to revoke.
pub(crate) fn profile(
    trusted: &[&TransportIdentity],
    subset: &[&TransportIdentity],
) -> ProfileConfig {
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

pub(crate) fn id() -> (ProfileIdentity, TransportIdentity) {
    let identity = ProfileIdentity::generate();
    let peer = identity.transport_identity().expect("peer id");
    (identity, peer)
}

/// A private state directory and the overlay's path in it.
pub(crate) fn state() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("a temporary directory");
    chmod(dir.path(), 0o700);
    let path = dir.path().join(TRUST_OVERLAY_FILE);
    (dir, path)
}

pub(crate) fn chmod(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).expect("chmod");
}

pub(crate) fn options(overlay: Option<&Path>) -> CompositionOptions {
    CompositionOptions {
        trust_overlay_file: overlay.map(Path::to_path_buf),
        ..CompositionOptions::default()
    }
}

pub(crate) async fn trust(runtime: &ComposedRuntime) -> TrustAdminView {
    runtime
        .sessions()
        .admin([AdminCapability::Trust].into())
        .await
        .expect("a port")
        .trust()
        .await
        .expect("the policy")
}

pub(crate) async fn set(
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

pub(crate) fn row(peer: &TransportIdentity, persisted: bool, source: TrustSource) -> TrustedPeer {
    TrustedPeer {
        peer: peer.clone(),
        persisted,
        source,
    }
}
