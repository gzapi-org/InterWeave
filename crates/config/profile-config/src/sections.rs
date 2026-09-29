// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The small top-level sections (plan §16 (13)): `identity`, `profile`
//! and `observability`.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::{ConfigError, ProfilePaths};

/// `identity.algorithm`: `literal[ed25519]`, fixed so portable backup and
/// restore are exact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IdentityAlgorithm {
    /// The one software identity algorithm.
    #[default]
    Ed25519,
}

/// `identity.key_protection`: `literal[filesystem-only]` in standard v1
/// (ADR-0038's encrypted envelope is not selectable until SPIKE-007).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum KeyProtection {
    /// Owner-only at rest, protected by the OS account.
    #[default]
    #[serde(rename = "filesystem-only")]
    FilesystemOnly,
}

/// `identity`. The private key itself is never configuration; recovery
/// phrases never appear here either.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct IdentityConfig {
    /// The key algorithm.
    pub algorithm: IdentityAlgorithm,
    /// `path?`: an override of where the key file lives.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key_file: Option<PathBuf>,
    /// How the key is protected at rest.
    pub key_protection: KeyProtection,
}

impl IdentityConfig {
    /// The key file this profile uses: the override when one is given,
    /// else the profile's identity file.
    ///
    /// Absent: the profile's identity file (ADR-0028's identity class).
    /// Absolute: as written. RELATIVE: joined to the profile's
    /// configuration directory -- where the document naming it lives, so
    /// a profile directory stays relocatable -- never the process's
    /// working directory, which changes with how the daemon was started
    /// (architect-cto, 2026-09-29; one test per branch in
    /// `the_key_file_resolves_against_the_profiles_configuration`).
    #[must_use]
    pub fn key_file_in(&self, paths: &ProfilePaths) -> PathBuf {
        match &self.key_file {
            None => paths.identity_file(),
            Some(path) if path.is_absolute() => path.clone(),
            Some(path) => paths.config_dir().join(path),
        }
    }
}

/// `profile`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileSection {
    /// `string[1..64]`: MUST equal the profile the document is loaded as
    /// (`ProfileConfig::load`); a mismatch is fatal.
    pub name: String,
}

/// `observability.log_level`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    /// Errors only.
    Error,
    /// Warnings and errors.
    Warn,
    /// The default.
    #[default]
    Info,
    /// Everything this build logs.
    Debug,
}

/// `observability`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ObservabilityConfig {
    /// The log level; nothing else sets it (plan §16 (11)).
    pub log_level: LogLevel,
    /// `literal[false]`: payload bytes are never logged.
    pub payload_logging: bool,
}

impl Default for ObservabilityConfig {
    fn default() -> Self {
        Self {
            log_level: LogLevel::Info,
            payload_logging: false,
        }
    }
}

/// Check the sections this module models.
pub(crate) fn validate_into(
    profile: Option<&ProfileSection>,
    observability: ObservabilityConfig,
    errors: &mut Vec<ConfigError>,
) {
    if let Some(section) = profile
        && crate::paths::validate_profile(&section.name).is_err()
    {
        errors.push(ConfigError::InvalidProfileName {
            name: section.name.clone(),
        });
    }
    if observability.payload_logging {
        errors.push(ConfigError::LiteralViolated {
            field: "observability.payload_logging",
        });
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]
    use super::*;
    use crate::{ProfileConfig, XdgRoots};

    fn doc(extra: &str) -> Result<ProfileConfig, serde_norway::Error> {
        serde_norway::from_str(&format!(
            "schema_version: 2\ntrust:\n  policy: static-allowlist\n  allowed_peers: []\nendpoints:\n  entries: []\n{extra}"
        ))
    }

    fn roots(base: &std::path::Path) -> XdgRoots {
        XdgRoots {
            config_home: base.join("config"),
            data_home: base.join("data"),
            state_home: base.join("state"),
            cache_home: base.join("cache"),
            runtime_dir: None,
        }
    }

    #[test]
    fn the_defaults_are_the_schemas_and_are_valid() {
        let p = doc("").expect("parses");
        assert_eq!(p.identity, IdentityConfig::default());
        assert_eq!(p.identity.algorithm, IdentityAlgorithm::Ed25519);
        assert_eq!(p.identity.key_protection, KeyProtection::FilesystemOnly);
        assert_eq!(p.observability.log_level, LogLevel::Info);
        assert!(!p.observability.payload_logging);
        assert!(p.profile.is_none());
        assert!(p.validate().is_empty(), "{:?}", p.validate());
    }

    #[test]
    fn the_literals_are_enforced() {
        for refused in [
            "identity:\n  algorithm: rsa\n",
            "identity:\n  key_protection: passphrase\n",
            "observability:\n  log_level: trace\n",
            "identity:\n  mnemonic: none\n",
            "observability:\n  sink: stderr\n",
            "profile:\n  name: a\n  owner: b\n",
            "profile: {}\n",
        ] {
            assert!(doc(refused).is_err(), "{refused} parsed");
        }
        for level in ["error", "warn", "info", "debug"] {
            assert!(doc(&format!("observability:\n  log_level: {level}\n")).is_ok());
        }
        let logging = doc("observability:\n  payload_logging: true\n").expect("parses");
        assert_eq!(
            logging.validate(),
            vec![ConfigError::LiteralViolated {
                field: "observability.payload_logging"
            }]
        );
    }

    /// The name must be a profile name the path layer accepts, since the
    /// loader compares it to one.
    #[test]
    fn the_profile_name_follows_the_path_grammar() {
        assert!(
            doc("profile:\n  name: work-2\n")
                .expect("parses")
                .validate()
                .is_empty()
        );
        for bad in ["\"\"", ".hidden", "a/b", "\"with space\""] {
            let p = doc(&format!("profile:\n  name: {bad}\n")).expect("parses");
            assert!(
                matches!(
                    p.validate().as_slice(),
                    [ConfigError::InvalidProfileName { .. }]
                ),
                "{bad}: {:?}",
                p.validate()
            );
        }
    }

    /// Absent, the profile's identity file; absolute, as written;
    /// relative, beside the profile's configuration -- never the working
    /// directory.
    #[test]
    fn the_key_file_resolves_against_the_profiles_configuration() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = ProfilePaths::resolve_offline("work", &roots(dir.path())).expect("paths");
        let absent = IdentityConfig::default();
        assert_eq!(absent.key_file_in(&paths), paths.identity_file());
        let absolute = IdentityConfig {
            key_file: Some(PathBuf::from("/srv/keys/work.key")),
            ..IdentityConfig::default()
        };
        assert_eq!(
            absolute.key_file_in(&paths),
            PathBuf::from("/srv/keys/work.key")
        );
        let relative = IdentityConfig {
            key_file: Some(PathBuf::from("keys/work.key")),
            ..IdentityConfig::default()
        };
        assert_eq!(
            relative.key_file_in(&paths),
            paths.config_dir().join("keys/work.key")
        );
        assert!(relative.key_file_in(&paths).is_absolute());
        // The working directory does not participate: the same override
        // resolves beside the configuration, never beside the process.
        let cwd = std::env::current_dir().expect("a working directory");
        assert_ne!(
            relative.key_file_in(&paths),
            cwd.join("keys/work.key"),
            "a relative key_file resolved against the working directory"
        );
    }
}
