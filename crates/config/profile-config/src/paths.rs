// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! Profile path resolution, and the separation ADR-0028 requires.
//!
//! Five roles, five directories:
//!
//! ```text
//! config:   $XDG_CONFIG_HOME/interweave/profiles/<profile>/config.yaml
//! identity: $XDG_DATA_HOME/interweave/profiles/<profile>/identity.key
//! state:    $XDG_STATE_HOME/interweave/profiles/<profile>/
//! cache:    $XDG_CACHE_HOME/interweave/profiles/<profile>/peers.json
//! run:      $XDG_RUNTIME_DIR/interweave/<profile>.sock
//! ```
//!
//! # Why the separation is load-bearing
//!
//! Each role has a different lifetime and a different backup policy.
//! Config is what a user edits and backs up; identity is
//! private-key-equivalent and must never ride along in an ordinary
//! backup; cache is safe to delete; state is mutable daemon data; run is
//! per-boot. Collapsing any two of them means one policy silently
//! applies to both — a cache clear that deletes the key, or a config
//! backup that publishes it.
//!
//! [`ProfilePaths::roles_are_distinct`] checks the separation actually
//! held after resolution, because the environment can point two XDG
//! variables at the same directory and nothing else would notice.

use std::path::{Path, PathBuf};

use crate::PersistError;

/// The namespace directory under each XDG root (ADR-0047).
pub const NAMESPACE: &str = "interweave";

/// Where profiles live under the namespace.
pub const PROFILES: &str = "profiles";

/// The human client's directory name under a profile's state root
/// ([`ProfilePaths::human_dir`]). Not the daemon's: its files there are
/// the profile lock alone, and that name is different.
pub const HUMAN_DIR: &str = "human";

/// A resolved set of paths for one profile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfilePaths {
    /// The profile name these paths belong to.
    profile: String,
    config_dir: PathBuf,
    identity_dir: PathBuf,
    state_dir: PathBuf,
    cache_dir: PathBuf,
    /// `None` when resolved offline: the identity tools and the lock
    /// need no runtime directory, and only the sockets live in one.
    run_dir: Option<PathBuf>,
}

/// The XDG base directories, already resolved.
///
/// Taken as a value rather than read inside the resolver so the whole
/// layout is testable without touching process environment — which two
/// tests running in parallel cannot safely share.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XdgRoots {
    /// `$XDG_CONFIG_HOME`, default `$HOME/.config`.
    pub config_home: PathBuf,
    /// `$XDG_DATA_HOME`, default `$HOME/.local/share`.
    pub data_home: PathBuf,
    /// `$XDG_STATE_HOME`, default `$HOME/.local/state`.
    pub state_home: PathBuf,
    /// `$XDG_CACHE_HOME`, default `$HOME/.cache`.
    pub cache_home: PathBuf,
    /// `$XDG_RUNTIME_DIR`. There is no safe default: see
    /// [`XdgRoots::from_env`].
    pub runtime_dir: Option<PathBuf>,
}

/// An environment value, kept only if it is an absolute path.
///
/// Extracted rather than inlined so it is testable: setting process
/// environment to test [`XdgRoots::from_env`] is `unsafe` in this
/// edition and races every other test in the binary, so a test that
/// "checked `from_env`" by building an `XdgRoots` by hand would be
/// checking its own fixture. This is the actual rule, and this is what
/// the test calls.
///
/// A relative value is dropped rather than resolved, because it would
/// resolve against the daemon's working directory — not a location any
/// user chose, and one that changes with how the daemon was started.
#[must_use]
pub fn absolute_or_none(value: Option<std::ffi::OsString>) -> Option<PathBuf> {
    value.map(PathBuf::from).filter(|p| p.is_absolute())
}

impl XdgRoots {
    /// Read the XDG roots from the process environment.
    ///
    /// A relative value in an XDG variable is IGNORED and the default
    /// used instead, as the specification requires — a relative path
    /// would resolve against the daemon's working directory, which is
    /// not a location any user chose.
    ///
    /// `XDG_RUNTIME_DIR` has no fallback on purpose. The specification's
    /// guarantees for that directory — owner-only, per-boot, per-user —
    /// are exactly what an IPC socket depends on, and inventing
    /// `/tmp/...` in its absence would silently drop them.
    ///
    /// # Errors
    /// Returns [`PersistError::MissingHome`] if `$HOME` is unset and a
    /// default is needed.
    pub fn from_env() -> Result<Self, PersistError> {
        let var = |name: &str| absolute_or_none(std::env::var_os(name));
        let home = || -> Result<PathBuf, PersistError> {
            absolute_or_none(std::env::var_os("HOME")).ok_or(PersistError::MissingHome)
        };

        Ok(Self {
            config_home: match var("XDG_CONFIG_HOME") {
                Some(p) => p,
                None => home()?.join(".config"),
            },
            data_home: match var("XDG_DATA_HOME") {
                Some(p) => p,
                None => home()?.join(".local").join("share"),
            },
            state_home: match var("XDG_STATE_HOME") {
                Some(p) => p,
                None => home()?.join(".local").join("state"),
            },
            cache_home: match var("XDG_CACHE_HOME") {
                Some(p) => p,
                None => home()?.join(".cache"),
            },
            runtime_dir: var("XDG_RUNTIME_DIR"),
        })
    }
}

/// Whether a profile name is safe to place in a path.
///
/// The name comes from a command line or a config file and is
/// concatenated into five filesystem paths, so `..`, a separator, or a
/// leading dot would let it escape or hide. Rejecting is the whole
/// defence — there is no sanitising step that turns `../../etc` into a
/// profile name someone meant.
pub(crate) fn validate_profile(name: &str) -> Result<(), PersistError> {
    let ok = !name.is_empty()
        && name.len() <= 64
        && !name.starts_with('.')
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'));
    if ok {
        Ok(())
    } else {
        Err(PersistError::InvalidProfileName {
            name: name.to_owned(),
        })
    }
}

impl ProfilePaths {
    /// Resolve the five roles for `profile` under `roots`, the runtime
    /// directory included: what the daemon needs.
    ///
    /// # Errors
    /// Returns [`PersistError::InvalidProfileName`] for a name that could
    /// escape or hide in a path, or [`PersistError::NoRuntimeDir`] if
    /// `XDG_RUNTIME_DIR` is unset.
    pub fn resolve(profile: &str, roots: &XdgRoots) -> Result<Self, PersistError> {
        let runtime = roots
            .runtime_dir
            .clone()
            .ok_or(PersistError::NoRuntimeDir)?;
        let mut paths = Self::resolve_offline(profile, roots)?;
        // NOT under profiles/: the socket path length is bounded by the
        // platform (sun_path is 108 bytes on Linux), and two extra path
        // components are two fewer a user's runtime directory can afford.
        paths.run_dir = Some(runtime.join(NAMESPACE));
        Ok(paths)
    }

    /// Resolve the offline roles -- configuration, identity, state and
    /// cache -- with no runtime directory: what the offline identity
    /// commands and the profile lock need (plan §16 (6), (10)). The
    /// socket paths are then unavailable, and ask for them answers
    /// [`PersistError::NoRuntimeDir`].
    ///
    /// # Errors
    /// Returns [`PersistError::InvalidProfileName`] for a name that could
    /// escape or hide in a path.
    pub fn resolve_offline(profile: &str, roots: &XdgRoots) -> Result<Self, PersistError> {
        validate_profile(profile)?;
        let under = |root: &Path| root.join(NAMESPACE).join(PROFILES).join(profile);
        Ok(Self {
            profile: profile.to_owned(),
            config_dir: under(&roots.config_home),
            identity_dir: under(&roots.data_home),
            state_dir: under(&roots.state_home),
            cache_dir: under(&roots.cache_home),
            run_dir: None,
        })
    }

    /// The profile these paths belong to.
    #[must_use]
    pub fn profile(&self) -> &str {
        &self.profile
    }

    /// The profile configuration file.
    #[must_use]
    pub fn config_file(&self) -> PathBuf {
        self.config_dir.join("config.yaml")
    }

    /// The private identity key file.
    ///
    /// Private-key-equivalent. Written owner-only, never backed up with
    /// configuration, and never logged.
    #[must_use]
    pub fn identity_file(&self) -> PathBuf {
        self.identity_dir.join("identity.key")
    }

    /// The replaceable peer cache.
    #[must_use]
    pub fn peer_cache_file(&self) -> PathBuf {
        self.cache_dir.join("peers.json")
    }

    /// The mutable daemon state directory.
    #[must_use]
    pub fn state_dir(&self) -> &Path {
        &self.state_dir
    }

    /// The human client's own directory: its store and its
    /// single-instance lock, under the profile's state root, apart from
    /// the daemon's derived paths (architect-cto's ruling, relay seq
    /// 11163). Derivation only: it creates nothing, and the store creates
    /// it owner-only and refuses a looser one.
    ///
    /// What keeps it apart is [`roles_are_distinct`](Self::roles_are_distinct),
    /// which refuses a layout where another derived role lands in or above
    /// it -- for a caller that asks. A configured `key_file` is not a role
    /// and is held apart where the profile is loaded instead:
    /// [`ProfileConfig::load`](crate::ProfileConfig::load) refuses one that
    /// resolves inside this directory, and validation refuses any that
    /// holds `..`. No binary calls this check at start yet: a client
    /// opening its store here should.
    #[must_use]
    pub fn human_dir(&self) -> PathBuf {
        self.state_dir.join(HUMAN_DIR)
    }

    /// The directory holding the identity key.
    #[must_use]
    pub fn identity_dir(&self) -> &Path {
        &self.identity_dir
    }

    /// The profile configuration directory.
    #[must_use]
    pub fn config_dir(&self) -> &Path {
        &self.config_dir
    }

    /// The peer cache directory.
    #[must_use]
    pub fn cache_dir(&self) -> &Path {
        &self.cache_dir
    }

    /// The data-plane IPC socket.
    ///
    /// # Errors
    /// [`PersistError::NoRuntimeDir`] for paths resolved offline.
    pub fn data_socket(&self) -> Result<PathBuf, PersistError> {
        self.socket(&format!("{}.sock", self.profile))
    }

    /// The administration IPC socket.
    ///
    /// A SEPARATE path, because the two boundaries carry different
    /// authority: a data connection can never obtain `admin.*`, and one
    /// socket serving both would make that a runtime check instead of a
    /// filesystem fact. `<profile>.admin.sock`: the `.` is outside the
    /// profile-name alphabet, so no profile's data socket can be another's
    /// admin socket (`LOCAL-IPC.md`, A 2026-10-01).
    ///
    /// # Errors
    /// [`PersistError::NoRuntimeDir`] for paths resolved offline.
    pub fn admin_socket(&self) -> Result<PathBuf, PersistError> {
        self.socket(&format!("{}.admin.sock", self.profile))
    }

    fn socket(&self, name: &str) -> Result<PathBuf, PersistError> {
        self.run_dir
            .as_ref()
            .map(|dir| dir.join(name))
            .ok_or(PersistError::NoRuntimeDir)
    }

    /// Whether the five roles really landed in five distinct places.
    ///
    /// The environment can point two XDG variables at the same
    /// directory, and nothing else would notice — the daemon would run
    /// perfectly while a cache clear deleted the identity key. Callers
    /// should refuse to start rather than proceed with a collapsed
    /// layout.
    ///
    /// The human client's directory sits inside the state role by design,
    /// so it is checked apart: no other role may be it, lie inside it, or
    /// contain it -- a data or cache root pointed into it would put the
    /// key or the cache among the human store's files.
    ///
    /// The comparison is lexical, so a role path holding a `..` component
    /// could name one place and compare as another: such a layout is
    /// refused outright rather than resolved.
    #[must_use]
    pub fn roles_are_distinct(&self) -> bool {
        let roles = [
            &self.config_dir,
            &self.identity_dir,
            &self.state_dir,
            &self.cache_dir,
        ]
        .into_iter()
        .chain(self.run_dir.as_ref());
        if roles.into_iter().any(|role| {
            role.components()
                .any(|c| c == std::path::Component::ParentDir)
        }) {
            return false;
        }
        let mut all = vec![
            &self.config_dir,
            &self.identity_dir,
            &self.state_dir,
            &self.cache_dir,
        ];
        all.extend(self.run_dir.as_ref());
        for (i, a) in all.iter().enumerate() {
            for b in all.iter().skip(i + 1) {
                if a == b {
                    return false;
                }
            }
        }
        let human = self.human_dir();
        let others = [&self.config_dir, &self.identity_dir, &self.cache_dir]
            .into_iter()
            .chain(self.run_dir.as_ref());
        for role in others {
            if role.starts_with(&human) || human.starts_with(role) {
                return false;
            }
        }
        true
    }
}
