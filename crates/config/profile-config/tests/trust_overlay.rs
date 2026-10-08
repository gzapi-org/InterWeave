// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The trust overlay on disk (ADR-0028 A 2026-10-07): every membership of
//! one peer in configured / added / revoked through load and each set,
//! and every reason a present overlay stops the daemon -- each beside the
//! control that the same file, made right, loads.

#![allow(clippy::expect_used, clippy::panic)]

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use interweave_profile_config::trust_overlay::{
    OverlayError, TRUST_OVERLAY_FILE, TrustOverlay, TrustSource,
};
use interweave_transport_api::TransportIdentity;
use interweave_trust_api::PeerTrustPolicy;

/// A synthetic Ed25519 `PeerId`, distinct for each `i`: the identity
/// multihash of a public-key protobuf whose key bytes start with `i`.
/// Test-only; no key exists behind it.
fn nth(i: u32) -> TransportIdentity {
    let mut bytes = [0u8; 38];
    bytes[..6].copy_from_slice(&[0x00, 0x24, 0x08, 0x01, 0x12, 0x20]);
    bytes[6..10].copy_from_slice(&i.to_be_bytes());
    TransportIdentity::parse(bs58::encode(bytes).into_string()).expect("a valid peer id")
}

fn set(peers: impl IntoIterator<Item = TransportIdentity>) -> BTreeSet<TransportIdentity> {
    peers.into_iter().collect()
}

/// A private state directory and the overlay's path in it.
fn state() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("a temporary directory");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700))
            .expect("chmod");
    }
    let path = dir.path().join(TRUST_OVERLAY_FILE);
    (dir, path)
}

/// Write `text` at `path` as the daemon would: owner-only.
fn put(path: &Path, text: &str) {
    std::fs::write(path, text).expect("written");
    chmod(path, 0o600);
}

fn chmod(path: &Path, mode: u32) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).expect("chmod");
    }
}

fn lists(added: &[&TransportIdentity], revoked: &[&TransportIdentity]) -> String {
    let ids = |l: &[&TransportIdentity]| {
        l.iter()
            .map(|p| format!("\"{}\"", p.as_str()))
            .collect::<Vec<_>>()
            .join(",")
    };
    format!(
        r#"{{"added":[{}],"revoked":[{}]}}"#,
        ids(added),
        ids(revoked)
    )
}

fn on_disk(path: &Path) -> serde_json::Value {
    serde_json::from_str(&std::fs::read_to_string(path).expect("read")).expect("json")
}

/// Every membership of one peer `p` in (configured, added, revoked) as
/// the file holds it: the two with `p` in both lists stop the load; the
/// other six load normalised -- an added peer the configuration lists
/// and a revoked one it does not are dropped and the file rewritten --
/// with `p` on the effective list exactly when (configured or added)
/// and not revoked. From each, an allow puts `p` on the list (its source
/// the configuration's if configured) and a revoke takes it off, by the
/// four moves.
#[test]
fn every_membership_loads_normalised_and_moves_by_the_four_rules() {
    let p = nth(1);
    let other = nth(2);
    for bits in 0u8..8 {
        let (c, a, r) = (bits & 1 != 0, bits & 2 != 0, bits & 4 != 0);
        let case = format!("configured={c} added={a} revoked={r}");
        let (_dir, path) = state();
        // `other` is configured in every case, so the configuration is
        // never empty and `p` is never the only peer it names.
        let configured = set(std::iter::once(other.clone()).chain(c.then(|| p.clone())));
        let added: Vec<&TransportIdentity> = a.then_some(&p).into_iter().collect();
        let revoked: Vec<&TransportIdentity> = r.then_some(&p).into_iter().collect();
        put(&path, &lists(&added, &revoked));

        let loaded = TrustOverlay::load(&path, &configured);
        if a && r {
            assert!(
                matches!(loaded, Err(OverlayError::InBothLists { ref peer }) if *peer == p),
                "{case}: {loaded:?}"
            );
            continue;
        }
        let (overlay, effective) = loaded.expect(&case);

        // Normalised: kept only what the configuration does not say.
        let keeps_added = a && !c;
        let keeps_revoked = r && c;
        assert_eq!(overlay.added().any(|x| *x == p), keeps_added, "{case}");
        assert_eq!(overlay.revoked().any(|x| *x == p), keeps_revoked, "{case}");
        let disk = on_disk(&path);
        assert_eq!(
            disk["added"].as_array().expect("added").len(),
            usize::from(keeps_added),
            "{case}: the rewrite"
        );
        assert_eq!(
            disk["revoked"].as_array().expect("revoked").len(),
            usize::from(keeps_revoked),
            "{case}: the rewrite"
        );

        let listed = (c || a) && !r;
        assert_eq!(effective.contains(&p), listed, "{case}");
        assert!(effective.contains(&other), "{case}: the control");
        assert_eq!(
            overlay.source(&configured, &p),
            listed.then_some(if c {
                TrustSource::Configured
            } else {
                TrustSource::Administered
            }),
            "{case}"
        );

        for allowed in [true, false] {
            let after = overlay
                .set(&configured, &p, allowed)
                .unwrap_or_else(|| overlay.clone());
            let now = after.effective(&configured).expect("bounded");
            assert_eq!(now.contains(&p), allowed, "{case}, set {allowed}");
            assert!(now.contains(&other), "{case}, set {allowed}: the control");
            assert_eq!(
                after.source(&configured, &p),
                allowed.then_some(if c {
                    TrustSource::Configured
                } else {
                    TrustSource::Administered
                }),
                "{case}, set {allowed}"
            );
            // The set changes the lists only when it changes the answer.
            assert_eq!(
                overlay.set(&configured, &p, allowed).is_some(),
                listed != allowed,
                "{case}, set {allowed}"
            );
            // A normalised overlay stays normalised.
            assert!(
                after.added().all(|x| !configured.contains(x))
                    && after.revoked().all(|x| configured.contains(x)),
                "{case}, set {allowed}"
            );
        }
    }
}

/// An absent overlay is the empty one, and loading does not create it.
#[test]
fn an_absent_overlay_is_empty_and_is_not_created() {
    let (_dir, path) = state();
    let configured = set([nth(1)]);
    let (overlay, effective) = TrustOverlay::load(&path, &configured).expect("loads");
    assert_eq!(overlay, TrustOverlay::default());
    assert_eq!(effective, configured);
    assert!(!path.exists());
}

/// The overlay is read under its directory as judged (ADR-0028 A
/// 2026-10-08): a state directory under a group-writable ancestor stops
/// the load, though the overlay itself is owner-only. The same file under
/// the ancestor at 0755 loading is the control.
#[cfg(target_os = "linux")]
#[test]
fn an_overlay_under_a_writable_ancestor_is_refused() {
    let root = tempfile::tempdir().expect("a temporary directory");
    chmod(root.path(), 0o700);
    let above = root.path().join("above");
    let state = above.join("state");
    std::fs::create_dir_all(&state).expect("mkdir");
    chmod(&above, 0o755);
    chmod(&state, 0o700);
    let path = state.join(TRUST_OVERLAY_FILE);
    let configured = set([nth(1)]);
    put(&path, &lists(&[&nth(2)], &[]));
    TrustOverlay::load(&path, &configured).expect("the control");
    chmod(&above, 0o775);
    match TrustOverlay::load(&path, &configured) {
        Err(OverlayError::NotPrivate { detail }) => {
            assert!(detail.contains("not sticky"), "{detail}");
        }
        other => panic!("refused: {other:?}"),
    }
}

/// A normalised overlay is not rewritten: the file loaded is the file
/// left, byte for byte.
#[test]
fn a_normalised_overlay_is_not_rewritten() {
    let (_dir, path) = state();
    let (p, q) = (nth(1), nth(2));
    let text = lists(&[&p], &[&q]);
    put(&path, &text);
    TrustOverlay::load(&path, &set([q])).expect("loads");
    assert_eq!(std::fs::read_to_string(&path).expect("read"), text);
}

/// A written overlay is owner-only and loads back as itself.
#[test]
fn a_written_overlay_is_owner_only_and_round_trips() {
    let (_dir, path) = state();
    let configured = set([nth(1)]);
    let overlay = TrustOverlay::default()
        .set(&configured, &nth(2), true)
        .and_then(|o| o.set(&configured, &nth(1), false))
        .expect("two moves");
    overlay.write(&path).expect("written");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(&path).expect("meta").permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    let (back, effective) = TrustOverlay::load(&path, &configured).expect("loads");
    assert_eq!(back, overlay);
    assert_eq!(effective, set([nth(2)]));
}

/// Each reason a present overlay stops the load, beside the control
/// that a well-formed private file of the same contents loads.
#[test]
fn a_present_overlay_that_cannot_be_trusted_stops_the_load() {
    let configured = set([nth(1)]);
    let good = lists(&[&nth(2)], &[]);

    // The control.
    let (_dir, path) = state();
    put(&path, &good);
    assert!(TrustOverlay::load(&path, &configured).is_ok());

    for (what, text) in [
        ("not json", "added: []"),
        (
            "an unknown field",
            r#"{"added":[],"revoked":[],"extra":[]}"#,
        ),
        ("a missing list", r#"{"added":[]}"#),
        ("not a peer id", r#"{"added":["nobody"],"revoked":[]}"#),
    ] {
        let (_dir, path) = state();
        put(&path, text);
        assert!(
            matches!(
                TrustOverlay::load(&path, &configured),
                Err(OverlayError::Parse(_))
            ),
            "{what}"
        );
    }

    #[cfg(unix)]
    for mode in [0o644, 0o620, 0o602, 0o640] {
        let (_dir, path) = state();
        put(&path, &good);
        chmod(&path, mode);
        assert!(
            matches!(
                TrustOverlay::load(&path, &configured),
                Err(OverlayError::NotPrivate { .. })
            ),
            "mode {mode:o}"
        );
    }

    #[cfg(unix)]
    {
        let (dir, path) = state();
        let real = dir.path().join("real.json");
        put(&real, &good);
        std::os::unix::fs::symlink(&real, &path).expect("a link");
        assert!(matches!(
            TrustOverlay::load(&path, &configured),
            Err(OverlayError::NotPrivate { .. })
        ));
    }

    {
        let (dir, path) = state();
        std::fs::create_dir(&path).expect("a directory where the file goes");
        chmod(&path, 0o700);
        let _ = dir;
        assert!(TrustOverlay::load(&path, &configured).is_err());
    }

    {
        let (_dir, path) = state();
        let padding = " ".repeat(usize::try_from(1024 * 1024 + 1).expect("fits"));
        put(&path, &format!("{good}{padding}"));
        assert!(matches!(
            TrustOverlay::load(&path, &configured),
            Err(OverlayError::TooLarge)
        ));
    }
}

/// The effective allowlist past the policy's bound stops the load:
/// refused, never truncated. One fewer is the control.
#[test]
fn an_effective_allowlist_past_the_bound_stops_the_load() {
    let max = u32::try_from(PeerTrustPolicy::MAX_ALLOWED_PEERS).expect("fits");
    let configured: BTreeSet<_> = (0..max - 1).map(nth).collect();
    for (extra, fits) in [(1u32, true), (2, false)] {
        let (_dir, path) = state();
        let added: Vec<TransportIdentity> = (max..max + extra).map(nth).collect();
        put(&path, &lists(&added.iter().collect::<Vec<_>>(), &[]));
        let loaded = TrustOverlay::load(&path, &configured);
        if fits {
            assert_eq!(
                loaded.expect("at the bound").1.len(),
                PeerTrustPolicy::MAX_ALLOWED_PEERS
            );
        } else {
            assert!(
                matches!(
                    loaded,
                    Err(OverlayError::EffectiveTooLarge { got }) if got == PeerTrustPolicy::MAX_ALLOWED_PEERS + 1
                ),
                "{loaded:?}"
            );
        }
    }
}

/// A list on disk longer than the allowlist's bound is refused as such,
/// before normalising could hide it -- a revoked list of any length
/// normalises down to the configuration -- and one at the bound loads
/// (the control). Measured on `revoked`, where only this check sees it.
#[test]
fn a_list_past_the_bound_on_disk_stops_the_load() {
    let max = u32::try_from(PeerTrustPolicy::MAX_ALLOWED_PEERS).expect("fits");
    let configured = set([nth(0)]);
    for (len, refused) in [(max, false), (max + 1, true)] {
        let (_dir, path) = state();
        let revoked: Vec<TransportIdentity> = (1..=len).map(nth).collect();
        put(&path, &lists(&[], &revoked.iter().collect::<Vec<_>>()));
        let loaded = TrustOverlay::load(&path, &configured);
        if refused {
            assert!(
                matches!(loaded, Err(OverlayError::ListTooLong)),
                "{loaded:?}"
            );
        } else {
            assert!(loaded.is_ok(), "{len}: {loaded:?}");
        }
    }
}

/// A normalisation rewrite that fails stops the load: the stale entry
/// left on disk could undo the operator's next `config.yaml` edit. The
/// same file in a writable directory is the control.
#[cfg(unix)]
#[test]
fn a_failed_normalisation_rewrite_stops_the_load() {
    let p = nth(1);
    let configured = set([p.clone()]);
    for writable in [true, false] {
        let (dir, path) = state();
        // `p` both added and configured: normalising drops it.
        put(&path, &lists(&[&p], &[]));
        if !writable {
            chmod(dir.path(), 0o500);
        }
        let loaded = TrustOverlay::load(&path, &configured);
        chmod(dir.path(), 0o700);
        if writable {
            assert!(loaded.is_ok(), "the control: {loaded:?}");
        } else {
            assert!(matches!(loaded, Err(OverlayError::Write(_))), "{loaded:?}");
            assert_eq!(on_disk(&path)["added"].as_array().expect("added").len(), 1);
        }
    }
}

/// An overlay that is a FIFO is refused as not a regular file, at once:
/// opened without `O_NONBLOCK` it blocked the load until a writer
/// appeared. The same lists as a file are the control. On a timeout the
/// FIFO is opened for writing, which releases the blocked load, before
/// the test fails.
#[cfg(target_os = "linux")]
#[test]
fn an_overlay_that_is_a_fifo_is_refused_without_waiting() {
    let configured = set([nth(1)]);
    let (_dir, path) = state();
    put(&path, &lists(&[&nth(2)], &[]));
    TrustOverlay::load(&path, &configured).expect("the control: a regular file");
    std::fs::remove_file(&path).expect("removed");
    let made = std::process::Command::new("mkfifo")
        .arg(&path)
        .status()
        .expect("mkfifo");
    assert!(made.success(), "mkfifo");
    chmod(&path, 0o600);
    let (tx, rx) = std::sync::mpsc::channel();
    let loading = path.clone();
    std::thread::spawn(move || {
        let _ = tx.send(TrustOverlay::load(&loading, &configured).map(drop));
    });
    let Ok(result) = rx.recv_timeout(std::time::Duration::from_secs(5)) else {
        let _ = std::fs::OpenOptions::new().write(true).open(&path);
        panic!("the load blocked on the FIFO");
    };
    match result {
        Err(OverlayError::NotPrivate { detail }) => {
            assert_eq!(detail, "it is not a regular file");
        }
        other => panic!("refused: {other:?}"),
    }
}
