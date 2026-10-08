// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//
// What every test binary of this crate shares.
#![allow(dead_code, clippy::expect_used)]

use std::path::Path;

/// A temporary directory made owner-only AT CREATION, whatever the umask.
///
/// `tempfile` creates with the process umask, so under umask 002 with a
/// shared primary group a bare `tempfile::tempdir()` is group-writable,
/// and the store's ancestor rule (ADR-0028 A 2026-10-08) refuses it: the
/// test fails at its setup, not at what it tests. CI runs umask 022 and
/// never sees it; a developer's host may not.
pub(crate) fn private_tempdir() -> tempfile::TempDir {
    private_tempdir_in(std::env::temp_dir())
}

/// [`private_tempdir`] under `dir`.
pub(crate) fn private_tempdir_in(dir: impl AsRef<Path>) -> tempfile::TempDir {
    let mut builder = tempfile::Builder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        builder.permissions(std::fs::Permissions::from_mode(0o700));
    }
    builder.tempdir_in(dir).expect("tempdir")
}
