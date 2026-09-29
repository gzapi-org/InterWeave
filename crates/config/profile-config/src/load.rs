// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! THE production YAML loader (plan §16 (13), precondition P5): the one
//! path by which a profile document becomes a `ProfileConfig`.
//!
//! [`ProfileConfig::parse_yaml`] turns text into the model -- every
//! section the schema declares, unknown keys refused at every level --
//! and judges nothing else. [`ProfileConfig::load`] is what a daemon or
//! the admin tool calls: it reads the profile's `config.yaml` under a
//! size ceiling, parses it, refuses a document whose `profile.name` is
//! not the profile it is being loaded as, and refuses one that fails
//! `validate()`.

use std::io::Read as _;

use crate::{ConfigError, ProfileConfig, ProfilePaths};

/// The largest profile document read, in bytes.
///
/// A profile is a few kilobytes; the largest one the schema allows --
/// 4096 allowed peers, 64 endpoints with their subsets, sixteen
/// discovery providers -- is a few hundred. A ceiling keeps a mistaken
/// path (a log, a device) from being read whole before the parser can
/// refuse it.
pub const MAX_PROFILE_BYTES: u64 = 1024 * 1024;

/// Why a profile document could not be loaded.
#[derive(Debug)]
pub enum LoadError {
    /// The document could not be read.
    Read(std::io::Error),
    /// The document is larger than [`MAX_PROFILE_BYTES`].
    TooLarge {
        /// The ceiling it exceeded.
        limit: u64,
    },
    /// The document is not a profile: malformed YAML, a missing required
    /// section, an unknown key, a value of the wrong type or a literal
    /// spelled otherwise.
    Parse(String),
    /// `profile.name` is absent or names another profile.
    NameMismatch {
        /// The profile being loaded.
        expected: String,
        /// What the document says, if anything.
        found: Option<String>,
    },
    /// The document parses and breaks the schema's rules.
    Invalid(Vec<ConfigError>),
}

impl core::fmt::Display for LoadError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Read(e) => write!(f, "the profile document cannot be read: {e}"),
            Self::TooLarge { limit } => {
                write!(f, "the profile document is larger than {limit} bytes")
            }
            Self::Parse(e) => write!(f, "the profile document is not a valid profile: {e}"),
            Self::NameMismatch { expected, found } => match found {
                Some(found) => write!(
                    f,
                    "profile.name is {found:?} but the profile loaded is {expected:?}"
                ),
                None => write!(
                    f,
                    "profile.name is missing; the profile loaded is {expected:?}"
                ),
            },
            Self::Invalid(errors) => {
                write!(f, "the profile breaks {} rule(s):", errors.len())?;
                for e in errors {
                    write!(f, " {e};")?;
                }
                Ok(())
            }
        }
    }
}

impl core::error::Error for LoadError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::Read(e) => Some(e),
            _ => None,
        }
    }
}

impl ProfileConfig {
    /// Parse a profile document. Nothing is validated beyond the shape.
    ///
    /// # Errors
    /// [`LoadError::Parse`] for anything that is not a profile.
    pub fn parse_yaml(text: &str) -> Result<Self, LoadError> {
        serde_norway::from_str(text).map_err(|e| LoadError::Parse(e.to_string()))
    }

    /// Load `paths`' profile document: read it (at most
    /// [`MAX_PROFILE_BYTES`]), parse it, require `profile.name` to be the
    /// profile `paths` resolves, and validate it.
    ///
    /// # Errors
    /// [`LoadError`], naming which of the four steps refused.
    pub fn load(paths: &ProfilePaths) -> Result<Self, LoadError> {
        let file = std::fs::File::open(paths.config_file()).map_err(LoadError::Read)?;
        let mut text = String::new();
        file.take(MAX_PROFILE_BYTES + 1)
            .read_to_string(&mut text)
            .map_err(LoadError::Read)?;
        if text.len() as u64 > MAX_PROFILE_BYTES {
            return Err(LoadError::TooLarge {
                limit: MAX_PROFILE_BYTES,
            });
        }
        let profile = Self::parse_yaml(&text)?;
        if profile.profile.as_ref().map(|p| p.name.as_str()) != Some(paths.profile()) {
            return Err(LoadError::NameMismatch {
                expected: paths.profile().to_owned(),
                found: profile.profile.map(|p| p.name),
            });
        }
        let errors = profile.validate();
        if errors.is_empty() {
            Ok(profile)
        } else {
            Err(LoadError::Invalid(errors))
        }
    }
}
