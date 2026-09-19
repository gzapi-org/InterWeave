// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The atomic writers against a medium that refuses the WRITE: the
//! temporary must not outlive the failure, whatever it held.
//!
//! The fault is `RLIMIT_FSIZE`, which std does not expose and this
//! crate will not reach through `unsafe` (`forbid(unsafe_code)`); so
//! the test re-runs itself as a child under `sh -c 'ulimit -f ...'` and
//! reads the child's verdict. The limit is per process.
//!
//! What it pins: a write that fails partway returns an error, leaves
//! the previous destination unchanged, and leaves NO temporary beside
//! it -- for `write_private_atomic` the temporary is the attempted
//! identity's key material, and each attempt names its own, so a
//! leftover was never cleaned by a retry (review finding, 2026-09-18).

#![cfg(unix)]
#![allow(clippy::expect_used, clippy::panic)]

use std::path::Path;
use std::process::Command;

use interweave_profile_config::persist::{write_atomic, write_private_atomic};

const CHILD_ENV: &str = "INTERWEAVE_WRITE_FAULT_DIR";
const CHILD_OK: &str = "WRITE-FAULT-CHILD-OK";
/// `ulimit -f` counts 512-byte blocks: 8 KiB. The control write below
/// fits; the payload the child attempts does not.
const LIMIT_BLOCKS: u32 = 16;

fn entries(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .expect("readable")
        .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

/// The half that runs under the limit.
fn under_the_fault(dir: &Path) {
    let too_big = vec![b'x'; 64 * 1024];
    for (name, write) in [
        (
            "profile.json",
            write_atomic as fn(&Path, &[u8]) -> Result<(), _>,
        ),
        ("identity.key", write_private_atomic),
    ] {
        let path = dir.join(name);
        let before = std::fs::read(&path).expect("the control wrote it");
        let refused = write(&path, &too_big);
        assert!(
            refused.is_err(),
            "{name}: a medium that cannot take the bytes refuses the write: {refused:?}"
        );
        assert_eq!(
            std::fs::read(&path).expect("still there"),
            before,
            "{name}: the previous destination is unchanged"
        );
    }
    assert_eq!(
        entries(dir),
        vec!["identity.key".to_owned(), "profile.json".to_owned()],
        "no temporary outlives a failed write"
    );
    println!("{CHILD_OK}");
}

#[test]
fn a_failed_write_leaves_no_temporary_behind() {
    if let Some(dir) = std::env::var_os(CHILD_ENV) {
        under_the_fault(Path::new(&dir));
        return;
    }

    let dir = tempfile::tempdir().expect("tempdir");
    let dir = dir.path().join("state");
    // THE CONTROL: with no fault, both writers publish, and the
    // directory holds exactly the two destinations. The private writer
    // first, so the directory is created owner-only as it requires.
    write_private_atomic(&dir.join("identity.key"), b"k").expect("an ordinary private write");
    write_atomic(&dir.join("profile.json"), b"{}").expect("an ordinary write");
    assert_eq!(
        entries(&dir),
        vec!["identity.key".to_owned(), "profile.json".to_owned()]
    );

    let me = std::env::current_exe().expect("this test binary");
    let output = Command::new("sh")
        .arg("-c")
        .arg(format!(
            "trap '' XFSZ; ulimit -f {LIMIT_BLOCKS} && exec \"$0\" \"$@\""
        ))
        .arg(&me)
        .args([
            "--exact",
            "a_failed_write_leaves_no_temporary_behind",
            "--nocapture",
        ])
        .env(CHILD_ENV, &dir)
        .output()
        .expect("the child runs");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success() && stdout.contains(CHILD_OK),
        "the child under the fault failed:\n--- stdout\n{stdout}\n--- stderr\n{stderr}"
    );
    // And the parent sees the same directory the child left.
    assert_eq!(
        entries(&dir),
        vec!["identity.key".to_owned(), "profile.json".to_owned()]
    );
}
