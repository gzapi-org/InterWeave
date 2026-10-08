// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! Whether a transport daemon serves the profile, asked through the
//! profile lock -- the facade cannot tell a missing daemon from any other
//! failed open.

use interweave_profile_config::{ProfileLock, ProfilePaths};

/// `Ok(true)` while a daemon process holds the profile: not a promise its
/// sockets are ready, it may be starting. `Ok(false)` when none held it
/// at that instant. `Err` when the lock cannot say -- a state directory
/// that is not private, say -- with why: the caller reports it and never
/// reads it as "no daemon".
///
/// # Errors
/// Why the lock cannot say.
pub fn present(paths: &ProfilePaths) -> Result<bool, String> {
    ProfileLock::is_held(paths).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;
    use std::time::Duration;

    use interweave_profile_config::{ProfileLock, ProfilePaths, XdgRoots};

    use super::present;

    fn paths(root: &std::path::Path) -> ProfilePaths {
        let roots = XdgRoots {
            config_home: root.join("config"),
            data_home: root.join("data"),
            state_home: root.join("state"),
            cache_home: root.join("cache"),
            runtime_dir: Some(root.join("run")),
        };
        ProfilePaths::resolve("desk", &roots).expect("paths")
    }

    #[test]
    fn held_absent_and_cannot_tell_are_three_answers() {
        // Owner-only at creation: under umask 002 a bare `tempdir()` is
        // group-writable and the lock refuses it as an ancestor.
        let dir = tempfile::Builder::new()
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir()
            .expect("tempdir");
        let paths = paths(dir.path());
        assert_eq!(present(&paths), Ok(false), "no lock file: no daemon");
        let held = ProfileLock::acquire(&paths, Duration::ZERO).expect("the daemon's lock");
        assert_eq!(present(&paths), Ok(true), "held: a daemon runs");
        drop(held);
        assert_eq!(present(&paths), Ok(false), "released");
        std::fs::set_permissions(paths.state_dir(), std::fs::Permissions::from_mode(0o755))
            .expect("widen");
        assert!(
            present(&paths).is_err(),
            "a state directory that is not private cannot say"
        );
    }
}
