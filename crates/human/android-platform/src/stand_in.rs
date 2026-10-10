// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! STAND-INS for what steps 2-4 run on before the parts that own them
//! land, compiled under the `dev-stand-ins` feature alone, which the app
//! turns on for its debug build -- without it there is no identity source
//! and the Service starts nothing:
//!
//! - the first-run profile: profile-config's own provisioning
//!   (`provision_embedded`, p2p-network-dev-01's, agreed in relay
//!   01a12499-ee78) replaces [`provision`];
//! - the identity: plan section 20 step 6's Keystore-wrapped key
//!   (p2p-network-dev-02's) replaces [`identity`].
//!
//! What a run on them proves stops short of both: a stand-in profile
//! trusts nobody, and its `PeerId` is new at every process start, so a
//! restart is a different peer. Nothing here writes a key anywhere.

use std::path::Path;

use interweave_profile_config::{
    PersistError, ProfilePaths, TrustBoundary, create_private_dir_within,
};
use interweave_profile_identity::ProfileIdentity;

/// The profile's document: the embedded deployment, wildcard listeners,
/// the human endpoint open to this client's kind, no IPC, and no trusted
/// peer, relay or AutoNAT server -- those name infrastructure a default
/// cannot know.
fn document(profile: &str) -> String {
    format!(
        "schema_version: 2
profile:
  name: {profile}
runtime:
  deployment: embedded-android
ipc:
  enabled: false
transport:
  listen:
    addresses: [\"/ip4/0.0.0.0/tcp/0\"]
trust:
  policy: static-allowlist
  allowed_peers: []
endpoints:
  registration_policy: configured-only
  default_direct_endpoint: human
  entries:
    - id: human
      enabled: true
      advertise: true
      allowed_client_kinds: [human-client]
      inbound: inherit_profile_trust
      outbound: inherit_profile_trust
channels:
  desired: []
"
    )
}

/// Write `profile`'s configuration under `app_data_dir` if there is none:
/// a profile once written is never rewritten, so what a person changed
/// survives.
///
/// # Errors
/// The directory or the file could not be made, or is refused by the
/// app's trust boundary.
pub fn provision(app_data_dir: &Path, profile: &str) -> Result<(), PersistError> {
    let paths = ProfilePaths::resolve_embedded(profile, TrustBoundary::new(app_data_dir)?)?;
    if paths.config_file().exists() {
        return Ok(());
    }
    create_private_dir_within(paths.config_dir(), paths.boundary())?;
    std::fs::write(paths.config_file(), document(profile)).map_err(PersistError::Io)
}

/// A fresh identity, held in memory only: a new `PeerId` at every start.
#[must_use]
pub fn identity() -> ProfileIdentity {
    ProfileIdentity::generate()
}
