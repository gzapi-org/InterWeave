// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! What the app needs before any window, and why it may not start: the
//! command line, the profile's paths and configuration, and the store.
//!
//! Every refusal here says what is wrong in words a person can act on,
//! and never carries message content (RETENTION.md §8: no shadow copy in
//! logs or error output).

use std::fmt;
use std::path::{Path, PathBuf};

use interweave_human_store::{HumanStore, StoreError, StoreOptions};
use interweave_profile_config::{LoadError, PersistError, ProfileConfig, ProfilePaths, XdgRoots};
use interweave_transport_api::{ChannelId, EndpointId};

/// The client kind this app presents, and the one the profile must allow
/// on the endpoint it leases.
pub const CLIENT_KIND: &str = "human-client";

/// The store's file name in the profile's human directory (architect-cto's
/// Q3 ruling, relay seq 11163).
pub const STORE_FILE: &str = "human.sqlite";

/// What the command line asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Launch {
    /// The profile whose daemon this client uses.
    pub profile: String,
    /// A page ceiling for the message store, when the person sets a
    /// quota: past it the store is full exactly as a full disk is, and the
    /// client degrades rather than accept unread content it cannot keep.
    pub store_max_pages: Option<u32>,
}

impl Launch {
    /// The store's options this launch asks for.
    #[must_use]
    pub fn store_options(&self) -> StoreOptions {
        StoreOptions {
            max_pages: self.store_max_pages,
        }
    }
}

/// Read the command line: `--profile <name>`, required, as the daemon's
/// is -- nothing names a default profile; and `--store-max-pages <n>`,
/// a positive page ceiling for the store, optional.
///
/// # Errors
/// The usage text, for anything else.
pub fn parse_args(args: impl IntoIterator<Item = String>) -> Result<Launch, Usage> {
    let mut args = args.into_iter();
    let mut profile = None;
    let mut store_max_pages = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--profile" => match args.next() {
                Some(name) if profile.is_none() => profile = Some(name),
                _ => return Err(Usage),
            },
            // Zero is refused here as a usage error: the store refuses it
            // too (`QuotaNotApplied`), but that reaches a person as "the
            // store could not be opened, try again", which is not what
            // is wrong.
            "--store-max-pages" => match args.next().and_then(|n| n.parse::<u32>().ok()) {
                Some(pages) if pages > 0 && store_max_pages.is_none() => {
                    store_max_pages = Some(pages);
                }
                _ => return Err(Usage),
            },
            _ => return Err(Usage),
        }
    }
    profile
        .map(|profile| Launch {
            profile,
            store_max_pages,
        })
        .ok_or(Usage)
}

/// A command line the app does not take.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Usage;

impl fmt::Display for Usage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("usage: human-desktop --profile <name> [--store-max-pages <pages>]")
    }
}

/// Everything the profile decides for this client.
#[derive(Debug, Clone)]
pub struct Profile {
    /// The profile's paths.
    pub paths: ProfilePaths,
    /// The endpoint this client leases: the profile's entry that allows
    /// the `human-client` kind. Configuration, never chosen in the UI
    /// (ADR-0032; architect-cto's Q8 ruling).
    pub endpoint: EndpointId,
    /// The channels the profile wants joined.
    pub channels: Vec<ChannelId>,
    /// The data socket.
    pub data_socket: PathBuf,
    /// The admin socket.
    pub admin_socket: PathBuf,
}

impl Profile {
    /// The store's path: in the profile's human directory, never among the
    /// daemon's files.
    #[must_use]
    pub fn store_path(&self) -> PathBuf {
        self.paths.human_dir().join(STORE_FILE)
    }
}

/// Why the app cannot start for this profile.
#[derive(Debug)]
pub enum StartError {
    /// The XDG directories could not be read.
    Directories(PersistError),
    /// The profile's paths could not be resolved.
    Paths(PersistError),
    /// Two of the profile's directories are one, or nest: the store could
    /// land among the daemon's files.
    RolesOverlap,
    /// The profile's configuration could not be loaded.
    Config(LoadError),
    /// No enabled endpoint in the profile allows this client's kind.
    NoHumanEndpoint,
    /// The socket paths could not be derived.
    Sockets(PersistError),
}

impl fmt::Display for StartError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Directories(e) => write!(f, "the XDG directories could not be read: {e}"),
            Self::Paths(e) => write!(f, "the profile's paths could not be resolved: {e}"),
            Self::RolesOverlap => f.write_str(
                "two of the profile's directories are the same or nest inside one another; \
                 refusing to put the message store where the daemon's files are",
            ),
            Self::Config(e) => write!(f, "the profile's configuration could not be loaded: {e}"),
            Self::NoHumanEndpoint => write!(
                f,
                "no enabled endpoint in the profile allows the {CLIENT_KIND} client kind"
            ),
            Self::Sockets(e) => write!(f, "the daemon's socket paths could not be derived: {e}"),
        }
    }
}

/// Resolve `launch`'s profile: its paths, the endpoint this client
/// leases, its channels and its sockets. Nothing is created here.
///
/// # Errors
/// [`StartError`], naming the step that refused.
pub fn resolve(launch: &Launch, roots: &XdgRoots) -> Result<Profile, StartError> {
    let paths = ProfilePaths::resolve(&launch.profile, roots).map_err(StartError::Paths)?;
    if !paths.roles_are_distinct() {
        return Err(StartError::RolesOverlap);
    }
    let config = ProfileConfig::load(&paths).map_err(StartError::Config)?;
    let endpoint = config
        .endpoints
        .entries
        .iter()
        .find(|e| {
            e.enabled
                && e.allowed_client_kinds
                    .iter()
                    .any(|k| k.as_str() == CLIENT_KIND)
        })
        .map(|e| e.id.clone())
        .ok_or(StartError::NoHumanEndpoint)?;
    Ok(Profile {
        endpoint,
        channels: config.channels.desired.clone(),
        data_socket: paths.data_socket().map_err(StartError::Sockets)?,
        admin_socket: paths.admin_socket().map_err(StartError::Sockets)?,
        paths,
    })
}

/// What opening the store came to.
#[derive(Debug)]
pub enum Opened {
    /// Open, at its current schema. A store over its quota opens too; the
    /// facade reports it degraded on its first turn.
    Ready(HumanStore),
    /// Not usable now: the app shows why, holds no session and no lease,
    /// and never renames, moves or deletes the file (architect-cto's Q4
    /// ruling).
    Blocked(Blocked),
}

/// Why the store cannot be used, as a person can act on it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Blocked {
    /// The file needs recovery: it is corrupt, or from a newer version, or
    /// its migration failed. Identity is never touched (STATE.md).
    Recovery {
        /// The file.
        path: PathBuf,
    },
    /// The file or its directory is not private to this user, or not a
    /// regular file: refused, never repaired.
    NotPrivate {
        /// The file.
        path: PathBuf,
    },
    /// The store could not be opened now; trying again may work.
    Unavailable {
        /// The file.
        path: PathBuf,
    },
}

impl fmt::Display for Blocked {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Recovery { path } => write!(
                f,
                "the message store at {} cannot be read by this version and needs recovery; \
                 it was not renamed, moved or deleted",
                path.display()
            ),
            Self::NotPrivate { path } => write!(
                f,
                "the message store at {} or its directory is not private to this user; \
                 refusing to use it",
                path.display()
            ),
            Self::Unavailable { path } => write!(
                f,
                "the message store at {} could not be opened now; try again, and if it \
                 persists check that the file, its -wal and -shm companions and its \
                 directory are writable and that there is space",
                path.display()
            ),
        }
    }
}

/// Open the store at `path`, classifying a failure by what a person can
/// do about it.
#[must_use]
pub fn open_store(path: &Path, options: StoreOptions) -> Opened {
    match HumanStore::open(path, options) {
        Ok(store) => Opened::Ready(store),
        Err(error) => Opened::Blocked(classify(path, &error)),
    }
}

fn classify(path: &Path, error: &StoreError) -> Blocked {
    let path = path.to_path_buf();
    if error.needs_recovery() {
        return Blocked::Recovery { path };
    }
    match error {
        StoreError::NotAFile { .. } | StoreError::PermissionsTooOpen { .. } => {
            Blocked::NotPrivate { path }
        }
        _ => Blocked::Unavailable { path },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn the_profile_is_required_and_nothing_else_is_taken() {
        assert_eq!(
            parse_args(args(&["--profile", "home"])),
            Ok(Launch {
                profile: "home".to_owned(),
                store_max_pages: None,
            })
        );
        assert_eq!(parse_args(args(&[])), Err(Usage));
        assert_eq!(parse_args(args(&["--profile"])), Err(Usage));
        assert_eq!(
            parse_args(args(&["--profile", "a", "--profile", "b"])),
            Err(Usage)
        );
        assert_eq!(parse_args(args(&["--endpoint", "human"])), Err(Usage));
    }

    #[test]
    fn a_store_quota_is_a_positive_page_count_given_once() {
        let launch =
            parse_args(args(&["--store-max-pages", "64", "--profile", "home"])).expect("parsed");
        assert_eq!(launch.store_max_pages, Some(64));
        assert_eq!(launch.store_options().max_pages, Some(64));
        assert_eq!(
            parse_args(args(&["--profile", "home"]))
                .expect("parsed")
                .store_options()
                .max_pages,
            None,
            "no quota unless asked"
        );
        for bad in [
            &["--profile", "home", "--store-max-pages"][..],
            &["--profile", "home", "--store-max-pages", "0"],
            &["--profile", "home", "--store-max-pages", "-1"],
            &["--profile", "home", "--store-max-pages", "lots"],
            &[
                "--profile",
                "home",
                "--store-max-pages",
                "8",
                "--store-max-pages",
                "9",
            ],
            &["--store-max-pages", "8"],
        ] {
            assert_eq!(parse_args(args(bad)), Err(Usage), "{bad:?}");
        }
    }

    #[test]
    fn a_store_failure_is_classified_by_what_a_person_can_do() {
        let path = Path::new("/x/human.sqlite");
        assert!(matches!(
            classify(path, &StoreError::Migration("newer".to_owned())),
            Blocked::Recovery { .. }
        ));
        assert!(matches!(
            classify(path, &StoreError::Corrupt("bad".to_owned())),
            Blocked::Recovery { .. }
        ));
        assert!(matches!(
            classify(path, &StoreError::Io(std::io::Error::other("busy"))),
            Blocked::Unavailable { .. }
        ));
    }
}
