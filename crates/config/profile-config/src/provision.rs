// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The embedded-android profile a fresh install starts from (plan §20
//! steps 2-4, the seam agreed with `rust-ui-dev`, `GZCoord` seq 95202).
//!
//! `EmbeddedHost::start` loads `config.yaml` from under the host's
//! configuration root, and on a fresh install nothing has written it:
//! the Service calls [`provision_embedded`] once, before the first start.
//!
//! The document is the minimum a host starts on, and the rest is the
//! schema's defaults: the embedded-android deployment with the `human`
//! endpoint it leases, a WILDCARD listener (the embedded-android rule,
//! `ConfigError::AndroidListenerNotWildcard`), the peer cache as its
//! only discovery, IPC off, and NO trust entries, relays or AutoNAT
//! servers -- those name operator infrastructure a default cannot know,
//! and arrive through the admin trust surface or onboarding. The
//! identity is not here: it is onboarding's (§20 step 6).

use crate::PersistError;
use crate::paths::ProfilePaths;
use crate::persist::create_private_exclusive_within;

/// The document [`provision_embedded`] writes for `profile`.
#[must_use]
pub fn embedded_document(profile: &str) -> String {
    format!(
        "# Written on first run by the Android client: the embedded-android
# defaults. Trust, relays and AutoNAT servers arrive through the admin
# trust surface or onboarding, not here.
schema_version: 2
profile:
  name: {profile}
runtime:
  deployment: embedded-android
  android:
    endpoint: human
ipc:
  enabled: false
transport:
  listen:
    addresses: [\"/ip4/0.0.0.0/tcp/0\"]
trust:
  policy: static-allowlist
  allowed_peers: []
endpoints:
  default_direct_endpoint: human
  entries:
    - id: human
      enabled: true
      advertise: true
discovery:
  providers:
    - type: peer-cache
      enabled: true
      priority: 10
      config: {{}}
"
    )
}

/// Write the embedded-android profile document for `paths`' profile at
/// its `config.yaml`, owner-only, under the trust boundary the paths
/// carry; REFUSED if one is already there, in the same filesystem
/// operation that installs it (`create_private_exclusive_within`), so a
/// second first run never replaces what the first wrote.
///
/// # Errors
/// [`PersistError::AlreadyExists`] when the profile is already
/// provisioned; otherwise what the write meets
/// (`create_private_exclusive_within`).
pub fn provision_embedded(paths: &ProfilePaths) -> Result<(), PersistError> {
    create_private_exclusive_within(
        &paths.config_file(),
        embedded_document(paths.profile()).as_bytes(),
        paths.boundary(),
    )
}
