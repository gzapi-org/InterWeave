// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The run directory and the two sockets (plan §16 (6); ADR-0037).
//!
//! The socket a connection arrived on IS its authority domain, so the two
//! listeners are kept apart from the bind onwards and never merged: a
//! connection is tagged by which one accepted it, before its first byte.
//!
//! [`bind`] refuses a path already present at a socket's place. Only the
//! profile lock's holder may replace a stale socket, and the lock is the
//! daemon's (plan §16 (6)): it calls [`bind_replacing_stale`], which
//! judges the run directory first, then replaces only this uid's socket
//! that no process is serving, and binds.

use std::fmt;
use std::fs::{DirBuilder, Metadata};
use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};

use interweave_ipc_protocol::AuthorityDomain;
use tokio::net::{UnixListener, UnixStream};

/// The directory both sockets live in: owner-only.
const RUN_DIR_MODE: u32 = 0o700;
/// Each socket: owner-only.
const SOCKET_MODE: u32 = 0o600;

/// Where the server binds. The daemon resolves these from the profile;
/// both sockets live directly in `run_dir`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SocketPaths {
    /// `<runtime>/interweave`: created 0700 if missing, else reused only
    /// when owned by this process's uid with mode 0700.
    pub run_dir: PathBuf,
    /// The data-plane socket.
    pub data: PathBuf,
    /// The administrative socket.
    pub admin: PathBuf,
}

/// Why binding failed.
#[derive(Debug)]
pub enum BindError {
    /// The run directory is not a directory owned by this process's uid
    /// with mode 0700 (a link included): fatal, `failure-model.md`'s
    /// "IPC bind security failure".
    RunDirNotPrivate {
        /// The directory.
        path: PathBuf,
        /// What it is instead.
        detail: String,
    },
    /// A socket path is not directly inside the run directory.
    SocketOutsideRunDir {
        /// The socket path.
        path: PathBuf,
    },
    /// Something already exists at a socket path. Removing a stale socket
    /// is the lock holder's decision, not this module's.
    PathExists {
        /// The socket path.
        path: PathBuf,
    },
    /// What is at a socket path is not a socket owned by this process's
    /// uid -- a file, a link, another uid's socket: fatal, and left as it
    /// was (`failure-model.md`: "a non-socket or foreign-owned path at a
    /// socket location").
    ForeignPath {
        /// The socket path.
        path: PathBuf,
        /// What it is instead.
        detail: String,
    },
    /// The operating system refused.
    Io {
        /// What was being done, and where.
        path: PathBuf,
        /// The error.
        source: std::io::Error,
    },
}

impl fmt::Display for BindError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RunDirNotPrivate { path, detail } => write!(
                f,
                "{} must be a directory of this user with mode 0700: {detail}",
                path.display()
            ),
            Self::SocketOutsideRunDir { path } => {
                write!(
                    f,
                    "{} is not directly inside the run directory",
                    path.display()
                )
            }
            Self::PathExists { path } => write!(
                f,
                "{} already exists; only the profile lock's holder removes a stale socket",
                path.display()
            ),
            Self::ForeignPath { path, detail } => write!(
                f,
                "{} is in a socket's place and is not this user's stale socket: {detail}",
                path.display()
            ),
            Self::Io { path, source } => write!(f, "{}: {source}", path.display()),
        }
    }
}

impl std::error::Error for BindError {}

/// The bound sockets.
#[derive(Debug)]
pub struct Listeners {
    pub(crate) data: UnixListener,
    /// An admin bind failure does not take the data socket down
    /// (ADR-0037): the daemon logs it and serves data alone.
    pub(crate) admin: Result<UnixListener, BindError>,
    /// The run directory's owner: the uid every peer must have.
    pub(crate) owner_uid: u32,
}

impl Listeners {
    /// The admin socket's bind failure, if it failed.
    #[must_use]
    pub fn admin_error(&self) -> Option<&BindError> {
        self.admin.as_ref().err()
    }

    /// The uid a connecting peer must have: the run directory's owner.
    #[must_use]
    pub const fn owner_uid(&self) -> u32 {
        self.owner_uid
    }

    /// The next connection, tagged with the domain of the socket that
    /// accepted it -- before anything is read from it.
    ///
    /// # Errors
    /// The accepting socket's error; the listener itself stays usable.
    pub async fn accept(&self) -> std::io::Result<(UnixStream, AuthorityDomain)> {
        let data = async {
            self.data
                .accept()
                .await
                .map(|(s, _)| (s, AuthorityDomain::Data))
        };
        match &self.admin {
            Ok(admin) => tokio::select! {
                accepted = data => accepted,
                accepted = admin.accept() => accepted.map(|(s, _)| (s, AuthorityDomain::Admin)),
            },
            Err(_) => data.await,
        }
    }
}

/// Bind both sockets in the run directory. Call from inside a tokio
/// runtime: the listeners register with its reactor.
///
/// # Errors
/// A [`BindError`] for the run directory or the data socket; an admin
/// socket failure is kept in [`Listeners`] instead.
pub fn bind(paths: &SocketPaths) -> Result<Listeners, BindError> {
    let owner_uid = prepare_run_dir(paths)?;
    let data = bind_socket(&paths.data)?;
    Ok(Listeners {
        data,
        admin: bind_socket(&paths.admin),
        owner_uid,
    })
}

/// [`bind`] for the profile lock's HOLDER only (plan §16 (6)): once the
/// run directory is judged, a stale socket of this uid in a socket's place
/// -- one whose connect is REFUSED, left by an earlier run -- is replaced.
/// Anything else there (a file, a link, another uid's socket, a socket
/// that accepts, or one whose connect fails any other way or does not
/// finish within a second) is [`BindError::ForeignPath`] and left
/// as it was.
///
/// # Errors
/// As [`bind`], and [`BindError::ForeignPath`] for the data socket's
/// place; the admin socket's is kept in [`Listeners`], as its bind failure
/// is.
pub async fn bind_replacing_stale(paths: &SocketPaths) -> Result<Listeners, BindError> {
    let owner_uid = prepare_run_dir(paths)?;
    clear_stale(&paths.data, owner_uid).await?;
    let data = bind_socket(&paths.data)?;
    let admin = match clear_stale(&paths.admin, owner_uid).await {
        Ok(()) => bind_socket(&paths.admin),
        Err(e) => Err(e),
    };
    Ok(Listeners {
        data,
        admin,
        owner_uid,
    })
}

/// How long [`bind_replacing_stale`] waits on its connect to a socket in
/// a socket's place. A Unix connect is answered at once -- accepted, or
/// refused with nobody listening, or `WouldBlock` with a full backlog --
/// so this bounds only the unforeseen; one that does not finish is not
/// proven stale.
const STALE_PROBE: std::time::Duration = std::time::Duration::from_secs(1);

/// Create the run directory if absent, judge it, and check both sockets
/// sit directly in it; this process's uid, which owns it, is returned.
fn prepare_run_dir(paths: &SocketPaths) -> Result<u32, BindError> {
    let io = |path: &Path| {
        let path = path.to_path_buf();
        move |source| BindError::Io { path, source }
    };
    let uid = effective_uid().map_err(io(&paths.run_dir))?;
    match std::fs::symlink_metadata(&paths.run_dir) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => DirBuilder::new()
            .mode(RUN_DIR_MODE)
            .create(&paths.run_dir)
            .map_err(io(&paths.run_dir))?,
        Err(e) => return Err(io(&paths.run_dir)(e)),
        Ok(_) => {}
    }
    let meta = std::fs::symlink_metadata(&paths.run_dir).map_err(io(&paths.run_dir))?;
    judge_run_dir(&paths.run_dir, &meta, uid)?;
    for socket in [&paths.data, &paths.admin] {
        if socket.parent() != Some(paths.run_dir.as_path()) {
            return Err(BindError::SocketOutsideRunDir {
                path: socket.clone(),
            });
        }
    }
    Ok(uid)
}

/// Refuse a run directory that is not a real directory of `uid` with mode
/// 0700. `symlink_metadata`, so a link to a private directory is still a
/// link.
fn judge_run_dir(path: &Path, meta: &Metadata, uid: u32) -> Result<(), BindError> {
    let refuse = |detail: String| BindError::RunDirNotPrivate {
        path: path.to_path_buf(),
        detail,
    };
    if !meta.file_type().is_dir() {
        return Err(refuse("not a directory".to_owned()));
    }
    if meta.uid() != uid {
        return Err(refuse(format!(
            "owned by uid {}, not this process's {uid}",
            meta.uid()
        )));
    }
    let mode = meta.mode() & 0o777;
    if mode != RUN_DIR_MODE {
        return Err(refuse(format!("mode {mode:o}")));
    }
    Ok(())
}

fn bind_socket(path: &Path) -> Result<UnixListener, BindError> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => {
            return Err(BindError::PathExists {
                path: path.to_path_buf(),
            });
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(source) => {
            return Err(BindError::Io {
                path: path.to_path_buf(),
                source,
            });
        }
    }
    let io = |source| BindError::Io {
        path: path.to_path_buf(),
        source,
    };
    let listener = UnixListener::bind(path).map_err(io)?;
    // The socket is created under the umask; the run directory is 0700,
    // so nobody else can reach it in the window before this.
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(SOCKET_MODE)).map_err(io)?;
    Ok(listener)
}

/// Clear a stale socket at `path`: this uid's socket whose connect is
/// refused. Being this uid's is not enough -- the lock proves only that no
/// daemon of THIS profile serves the path, and any process of this uid can
/// bind there -- so only a refusal, which says nobody listens, proves it
/// stale. A connect that fails otherwise (`EACCES` from a socket at mode
/// 0, `WouldBlock` from a full backlog) proves nothing either way, and the
/// path is left.
async fn clear_stale(path: &Path, uid: u32) -> Result<(), BindError> {
    let io = |source| BindError::Io {
        path: path.to_path_buf(),
        source,
    };
    let meta = match std::fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(io(e)),
    };
    judge_stale(path, &meta, uid)?;
    let refuse = |detail: String| BindError::ForeignPath {
        path: path.to_path_buf(),
        detail,
    };
    // tokio's connect is non-blocking: a full backlog is `WouldBlock` at
    // once, where std's would wait for a slot with the lock held.
    match tokio::time::timeout(STALE_PROBE, UnixStream::connect(path)).await {
        Ok(Err(e)) if e.kind() == std::io::ErrorKind::ConnectionRefused => {
            std::fs::remove_file(path).map_err(io)
        }
        Ok(Ok(_)) => Err(refuse(
            "a live socket: another process is serving it".into(),
        )),
        Ok(Err(e)) => Err(refuse(format!(
            "not provably stale: connecting failed ({e})"
        ))),
        Err(_) => Err(refuse(format!(
            "not provably stale: connecting did not finish within {STALE_PROBE:?}"
        ))),
    }
}

/// Whether `meta` is a socket owned by `uid`: the one thing a lock holder
/// may unlink.
fn judge_stale(path: &Path, meta: &Metadata, uid: u32) -> Result<(), BindError> {
    use std::os::unix::fs::FileTypeExt as _;
    let refuse = |detail: String| BindError::ForeignPath {
        path: path.to_path_buf(),
        detail,
    };
    if !meta.file_type().is_socket() {
        return Err(refuse(format!("not a socket ({:?})", meta.file_type())));
    }
    if meta.uid() != uid {
        return Err(refuse(format!("a socket owned by uid {}", meta.uid())));
    }
    Ok(())
}

/// This process's effective uid, read safely: the peer credential of one
/// end of a socket pair this process made is this process's own.
fn effective_uid() -> std::io::Result<u32> {
    let (one, _other) = std::os::unix::net::UnixStream::pair()?;
    let one = tokio::net::UnixStream::from_std({
        one.set_nonblocking(true)?;
        one
    })?;
    Ok(one.peer_cred()?.uid())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]
    use super::*;

    fn paths(root: &Path) -> SocketPaths {
        let run_dir = root.join("interweave");
        SocketPaths {
            data: run_dir.join("data.sock"),
            admin: run_dir.join("admin.sock"),
            run_dir,
        }
    }

    fn mode(path: &Path) -> u32 {
        std::fs::symlink_metadata(path).expect("meta").mode() & 0o777
    }

    #[tokio::test]
    async fn both_sockets_are_bound_owner_only_in_an_owner_only_directory() {
        let root = tempfile::tempdir().expect("tempdir");
        let paths = paths(root.path());
        let bound = bind(&paths).expect("binds");
        assert_eq!(mode(&paths.run_dir), 0o700);
        assert_eq!(mode(&paths.data), 0o600);
        assert_eq!(mode(&paths.admin), 0o600);
        assert!(bound.admin_error().is_none());
        assert_eq!(bound.owner_uid(), effective_uid().expect("uid"));
    }

    #[tokio::test]
    async fn a_run_directory_wider_than_owner_only_or_a_link_is_refused() {
        let root = tempfile::tempdir().expect("tempdir");
        let paths = paths(root.path());
        std::fs::create_dir(&paths.run_dir).expect("mkdir");
        std::fs::set_permissions(&paths.run_dir, std::fs::Permissions::from_mode(0o755))
            .expect("chmod");
        assert!(matches!(
            bind(&paths),
            Err(BindError::RunDirNotPrivate { .. })
        ));

        let real = root.path().join("real");
        DirBuilder::new().mode(0o700).create(&real).expect("mkdir");
        let linked = SocketPaths {
            run_dir: root.path().join("link"),
            data: root.path().join("link/data.sock"),
            admin: root.path().join("link/admin.sock"),
        };
        std::os::unix::fs::symlink(&real, &linked.run_dir).expect("link");
        assert!(matches!(
            bind(&linked),
            Err(BindError::RunDirNotPrivate { .. })
        ));
        assert!(
            std::fs::read_dir(&real).expect("list").next().is_none(),
            "nothing was bound through the link"
        );
    }

    /// Another uid's directory cannot be made unprivileged, so the judge
    /// is fed a real directory and a uid that is not its owner.
    #[test]
    fn a_run_directory_of_another_uid_is_refused() {
        let root = tempfile::tempdir().expect("tempdir");
        let dir = root.path().join("d");
        DirBuilder::new().mode(0o700).create(&dir).expect("mkdir");
        let meta = std::fs::symlink_metadata(&dir).expect("meta");
        assert!(
            judge_run_dir(&dir, &meta, meta.uid()).is_ok(),
            "the control"
        );
        assert!(matches!(
            judge_run_dir(&dir, &meta, meta.uid().wrapping_add(1)),
            Err(BindError::RunDirNotPrivate { .. })
        ));
    }

    #[tokio::test]
    async fn an_existing_path_is_left_to_the_lock_holder() {
        let root = tempfile::tempdir().expect("tempdir");
        let paths = paths(root.path());
        DirBuilder::new()
            .mode(0o700)
            .create(&paths.run_dir)
            .expect("mkdir");
        std::fs::write(&paths.data, b"not a socket").expect("plant");
        assert!(matches!(bind(&paths), Err(BindError::PathExists { .. })));
        assert_eq!(
            std::fs::read(&paths.data).expect("read"),
            b"not a socket",
            "left as it was"
        );
    }

    #[tokio::test]
    async fn a_stale_socket_of_this_uid_is_replaced_by_the_lock_holder() {
        let root = tempfile::tempdir().expect("tempdir");
        let paths = paths(root.path());
        DirBuilder::new()
            .mode(0o700)
            .create(&paths.run_dir)
            .expect("mkdir");
        // A socket left by an earlier run: bound, then its listener gone.
        drop(std::os::unix::net::UnixListener::bind(&paths.data).expect("an old socket"));
        assert!(
            matches!(bind(&paths), Err(BindError::PathExists { .. })),
            "a plain bind leaves it to the lock holder"
        );
        let bound = bind_replacing_stale(&paths)
            .await
            .expect("replaced and bound");
        assert!(bound.admin_error().is_none());
        assert_eq!(mode(&paths.data), 0o600);
    }

    #[tokio::test]
    async fn anything_but_this_uids_idle_socket_is_refused_and_left() {
        let root = tempfile::tempdir().expect("tempdir");
        let paths = paths(root.path());
        DirBuilder::new()
            .mode(0o700)
            .create(&paths.run_dir)
            .expect("mkdir");
        // A LIVE socket of this uid: another process serves it.
        let live = std::os::unix::net::UnixListener::bind(&paths.data).expect("a live socket");
        assert!(matches!(
            bind_replacing_stale(&paths).await,
            Err(BindError::ForeignPath { .. })
        ));
        assert!(
            std::os::unix::net::UnixStream::connect(&paths.data).is_ok(),
            "still served"
        );
        drop(live);
        std::fs::remove_file(&paths.data).expect("cleared");
        // A file.
        std::fs::write(&paths.data, b"not a socket").expect("plant");
        assert!(matches!(
            bind_replacing_stale(&paths).await,
            Err(BindError::ForeignPath { .. })
        ));
        assert_eq!(std::fs::read(&paths.data).expect("read"), b"not a socket");
        std::fs::remove_file(&paths.data).expect("cleared");
        // A link to a socket is judged as the link, not followed; on the
        // admin place it is kept as the admin error, the data bound.
        let target = root.path().join("real.sock");
        drop(std::os::unix::net::UnixListener::bind(&target).expect("a socket"));
        std::os::unix::fs::symlink(&target, &paths.admin).expect("link");
        let bound = bind_replacing_stale(&paths).await.expect("the data binds");
        assert!(matches!(
            bound.admin_error(),
            Some(BindError::ForeignPath { .. })
        ));
        assert!(target.exists(), "the link's target untouched");
        // Another uid's socket: judged without privileges to make one.
        let meta = std::fs::symlink_metadata(&target).expect("meta");
        assert!(matches!(
            judge_stale(&target, &meta, meta.uid().wrapping_add(1)),
            Err(BindError::ForeignPath { .. })
        ));
        assert!(judge_stale(&target, &meta, meta.uid()).is_ok());
    }

    /// Only a REFUSED connect proves a socket stale: one at mode 0 that a
    /// process serves answers `EACCES`, and a full backlog `WouldBlock`
    /// -- each is left, and the second is answered at once rather than
    /// waited on with the lock held.
    #[tokio::test]
    async fn a_socket_whose_connect_fails_otherwise_than_refused_is_left() {
        let root = tempfile::tempdir().expect("tempdir");
        let paths = paths(root.path());
        DirBuilder::new()
            .mode(0o700)
            .create(&paths.run_dir)
            .expect("mkdir");

        // Served, at mode 0.
        let live = std::os::unix::net::UnixListener::bind(&paths.data).expect("a live socket");
        std::fs::set_permissions(&paths.data, std::fs::Permissions::from_mode(0)).expect("0000");
        assert!(
            std::os::unix::net::UnixStream::connect(&paths.data).is_err(),
            "the control: a connect to it fails (run as a user, not root)"
        );
        assert!(matches!(
            bind_replacing_stale(&paths).await,
            Err(BindError::ForeignPath { .. })
        ));
        std::fs::set_permissions(&paths.data, std::fs::Permissions::from_mode(0o600))
            .expect("0600");
        assert!(
            std::os::unix::net::UnixStream::connect(&paths.data).is_ok(),
            "still there and still served"
        );
        drop(live);
        std::fs::remove_file(&paths.data).expect("cleared");

        // Served, never accepting, its backlog full.
        let socket = tokio::net::UnixSocket::new_stream().expect("a socket");
        socket.bind(&paths.data).expect("binds");
        let _full = socket.listen(1).expect("listens");
        let mut queued = Vec::new();
        let filled = loop {
            assert!(queued.len() < 64, "the backlog never filled");
            match UnixStream::connect(&paths.data).await {
                Ok(stream) => queued.push(stream),
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break true,
                Err(e) => panic!("filling the backlog: {e}"),
            }
        };
        assert!(filled);
        let judged = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            bind_replacing_stale(&paths),
        )
        .await
        .expect("answered at once, not waited on");
        assert!(matches!(judged, Err(BindError::ForeignPath { .. })));
        assert!(paths.data.exists(), "the full socket is left");
    }

    #[tokio::test]
    async fn a_run_directory_is_judged_before_any_stale_socket_is_touched() {
        let root = tempfile::tempdir().expect("tempdir");
        let paths = paths(root.path());
        // Wider than owner-only, with an idle socket of this uid in it.
        DirBuilder::new()
            .mode(0o755)
            .create(&paths.run_dir)
            .expect("mkdir");
        std::fs::set_permissions(&paths.run_dir, std::fs::Permissions::from_mode(0o755))
            .expect("0755");
        drop(std::os::unix::net::UnixListener::bind(&paths.data).expect("an old socket"));
        assert!(matches!(
            bind_replacing_stale(&paths).await,
            Err(BindError::RunDirNotPrivate { .. })
        ));
        assert!(paths.data.exists(), "nothing in it was touched");
    }

    #[tokio::test]
    async fn an_admin_bind_failure_leaves_the_data_socket_bound() {
        let root = tempfile::tempdir().expect("tempdir");
        let paths = paths(root.path());
        DirBuilder::new()
            .mode(0o700)
            .create(&paths.run_dir)
            .expect("mkdir");
        std::fs::write(&paths.admin, b"").expect("plant");
        let bound = bind(&paths).expect("the data socket binds");
        assert!(matches!(
            bound.admin_error(),
            Some(BindError::PathExists { .. })
        ));
        assert_eq!(mode(&paths.data), 0o600);
    }

    /// The domain is the socket's, and nothing the client sends decides
    /// it: a connection to each is tagged by which one accepted it.
    #[tokio::test]
    async fn a_connection_is_tagged_by_the_socket_that_accepted_it() {
        let root = tempfile::tempdir().expect("tempdir");
        let paths = paths(root.path());
        let bound = bind(&paths).expect("binds");
        for (path, domain) in [
            (&paths.data, AuthorityDomain::Data),
            (&paths.admin, AuthorityDomain::Admin),
            (&paths.data, AuthorityDomain::Data),
        ] {
            let _client = UnixStream::connect(path).await.expect("connects");
            let (_, got) = bound.accept().await.expect("accepts");
            assert_eq!(got, domain, "{}", path.display());
        }
    }

    #[tokio::test]
    async fn a_socket_outside_the_run_directory_is_refused() {
        let root = tempfile::tempdir().expect("tempdir");
        let mut paths = paths(root.path());
        paths.admin = root.path().join("admin.sock");
        assert!(matches!(
            bind(&paths),
            Err(BindError::SocketOutsideRunDir { .. })
        ));
    }
}
