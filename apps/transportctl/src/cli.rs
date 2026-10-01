// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! `transportctl`'s command line (plan §16 (10), (11): no `clap`, a small
//! tested parser).
//!
//! The admin commands speak to a running daemon over its admin socket;
//! the identity commands never touch a socket (ADR-0033) and are parsed
//! here as their own family. A recovery phrase is never an argument --
//! nothing in this grammar can carry one. And no refusal repeats a value
//! it was given -- a phrase typed in the wrong place would otherwise be
//! copied into whatever logs stderr.

use std::path::PathBuf;
use std::time::Duration;

use interweave_transport_api::{EndpointId, TransportIdentity};

/// The usage text.
pub(crate) const USAGE: &str = "\
usage:
  transportctl --profile <name> status [--json]
  transportctl --profile <name> endpoints list [--json]
  transportctl --profile <name> endpoints revoke|enable|disable <endpoint>
  transportctl --profile <name> endpoints default <endpoint>|--none
  transportctl --profile <name> shutdown [--grace <ms>]
  transportctl --profile <name> identity backup [--to-file <new path>]
  transportctl identity verify [--expected-peer-id <peer>]
  transportctl --profile <name> identity restore --new [--expected-peer-id <peer>]
  transportctl --profile <name> identity restore --replace --replacing <peer> [--expected-peer-id <peer>]

The recovery phrase, or a recovery record, is read from stdin -- hidden on a terminal.";

/// What the command line asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Command {
    /// Print the usage and exit 0.
    Help,
    /// An admin method against the profile's running daemon.
    Admin {
        /// The profile whose daemon is asked.
        profile: String,
        /// The method.
        action: Admin,
        /// Print the method's result object as JSON.
        json: bool,
    },
    /// An offline identity command.
    Identity(Identity),
}

/// An admin method (plan §16 (10)).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Admin {
    /// `admin.status`.
    Status,
    /// `admin.endpoints.list`.
    EndpointsList,
    /// `admin.endpoints.revoke`.
    Revoke(EndpointId),
    /// `admin.endpoints.set_enabled`.
    SetEnabled(EndpointId, bool),
    /// `admin.endpoints.set_default`; `None` clears it.
    SetDefault(Option<EndpointId>),
    /// `admin.shutdown`, with the grace if one was given.
    Shutdown(Option<Duration>),
}

/// An offline identity command: never over IPC (ADR-0033).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Identity {
    /// Emit the profile's recovery record.
    Backup {
        /// The profile whose key is backed up.
        profile: String,
        /// A new file to write the record to, instead of the terminal.
        to_file: Option<PathBuf>,
    },
    /// Check a phrase against a `PeerId`, writing nothing.
    Verify {
        /// The `PeerId` the phrase must restore, when no record names it.
        expected: Option<TransportIdentity>,
    },
    /// Restore the profile's key from a phrase.
    Restore {
        /// The profile restored into.
        profile: String,
        /// Into an empty profile, or over an established one.
        mode: RestoreMode,
        /// The `PeerId` the phrase must restore, when no record names it.
        expected: Option<TransportIdentity>,
    },
}

/// How a restore meets what is stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RestoreMode {
    /// Into a profile with no key (`restore_new`).
    New,
    /// Over an established key, which must be `replacing`
    /// (`restore_replace`, IDENTITY-RECOVERY.md restore item 8).
    Replace {
        /// The identity the stored key must be.
        replacing: TransportIdentity,
    },
}

/// The flags, once collected; each command takes what it accepts and
/// refuses the rest.
#[derive(Default)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each bool is one switch on the command line, not a state; a command takes the ones it accepts"
)]
struct Flags {
    profile: Option<String>,
    json: bool,
    none: bool,
    grace: Option<String>,
    to_file: Option<String>,
    expected: Option<String>,
    replacing: Option<String>,
    new: bool,
    replace: bool,
}

/// Parse the arguments after the program name.
///
/// # Errors
/// A message naming what is wrong: exit code 2.
pub(crate) fn parse<I: IntoIterator<Item = String>>(args: I) -> Result<Command, String> {
    let mut flags = Flags::default();
    let mut words = Vec::new();
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        let mut value = |slot: &mut Option<String>, name: &str| -> Result<(), String> {
            let v = args.next().ok_or_else(|| format!("{name} needs a value"))?;
            if slot.replace(v).is_some() {
                return Err(format!("{name} given twice"));
            }
            Ok(())
        };
        match arg.as_str() {
            "--help" | "-h" => return Ok(Command::Help),
            "--profile" => value(&mut flags.profile, "--profile")?,
            "--grace" => value(&mut flags.grace, "--grace")?,
            "--to-file" => value(&mut flags.to_file, "--to-file")?,
            "--expected-peer-id" => value(&mut flags.expected, "--expected-peer-id")?,
            "--replacing" => value(&mut flags.replacing, "--replacing")?,
            "--json" => flags.json = true,
            "--none" => flags.none = true,
            "--new" => flags.new = true,
            "--replace" => flags.replace = true,
            flag if flag.starts_with('-') => return Err("an unknown flag".to_owned()),
            _ => words.push(arg),
        }
    }
    let words: Vec<&str> = words.iter().map(String::as_str).collect();
    match words.as_slice() {
        ["identity", rest @ ..] => identity(rest, flags).map(Command::Identity),
        [] => Err("a command is required".to_owned()),
        _ => admin(&words, flags),
    }
}

fn admin(words: &[&str], mut flags: Flags) -> Result<Command, String> {
    let profile = flags.profile.take().ok_or("--profile <name> is required")?;
    let endpoint = |id: &str| EndpointId::parse(id).map_err(|e| format!("the endpoint: {e}"));
    let (action, json_allowed) = match words {
        ["status"] => (Admin::Status, true),
        ["endpoints", "list"] => (Admin::EndpointsList, true),
        ["endpoints", "revoke", id] => (Admin::Revoke(endpoint(id)?), false),
        ["endpoints", "enable", id] => (Admin::SetEnabled(endpoint(id)?, true), false),
        ["endpoints", "disable", id] => (Admin::SetEnabled(endpoint(id)?, false), false),
        ["endpoints", "default"] if std::mem::take(&mut flags.none) => {
            (Admin::SetDefault(None), false)
        }
        ["endpoints", "default", id] => (Admin::SetDefault(Some(endpoint(id)?)), false),
        ["shutdown"] => {
            let grace = flags
                .grace
                .take()
                .map(|ms| {
                    ms.parse::<u64>()
                        .map(Duration::from_millis)
                        .map_err(|_| "--grace: not a number of milliseconds".to_owned())
                })
                .transpose()?;
            (Admin::Shutdown(grace), false)
        }
        _ => return Err("an unknown command (see --help)".to_owned()),
    };
    let json = std::mem::take(&mut flags.json);
    if json && !json_allowed {
        return Err("--json applies to status and endpoints list only".to_owned());
    }
    refuse_leftovers(&flags)?;
    Ok(Command::Admin {
        profile,
        action,
        json,
    })
}

fn identity(words: &[&str], mut flags: Flags) -> Result<Identity, String> {
    let peer =
        |id: String, flag: &str| TransportIdentity::parse(&id).map_err(|e| format!("{flag}: {e}"));
    let expected = flags
        .expected
        .take()
        .map(|id| peer(id, "--expected-peer-id"))
        .transpose()?;
    let command = match words {
        ["backup"] => {
            if expected.is_some() {
                return Err("--expected-peer-id does not apply to backup".to_owned());
            }
            Identity::Backup {
                profile: flags.profile.take().ok_or("--profile <name> is required")?,
                to_file: flags.to_file.take().map(PathBuf::from),
            }
        }
        // No profile: verify reads no key and writes nothing, so it has no
        // profile to name (IDENTITY-RECOVERY.md §Verify-only).
        ["verify"] => Identity::Verify { expected },
        ["restore"] => {
            let profile = flags.profile.take().ok_or("--profile <name> is required")?;
            let mode = match (
                std::mem::take(&mut flags.new),
                std::mem::take(&mut flags.replace),
                flags.replacing.take(),
            ) {
                (true, false, None) => RestoreMode::New,
                (false, true, Some(replacing)) => RestoreMode::Replace {
                    replacing: peer(replacing, "--replacing")?,
                },
                (false, true, None) => {
                    return Err("--replace needs --replacing <the stored peer id>".to_owned());
                }
                _ => return Err("restore takes exactly one of --new or --replace".to_owned()),
            };
            Identity::Restore {
                profile,
                mode,
                expected,
            }
        }
        _ => return Err("an unknown identity command (see --help)".to_owned()),
    };
    refuse_leftovers(&flags)?;
    Ok(command)
}

/// A flag the chosen command does not take is a usage error, never
/// silently ignored.
fn refuse_leftovers(flags: &Flags) -> Result<(), String> {
    let given = [
        ("--profile", flags.profile.is_some()),
        ("--json", flags.json),
        ("--none", flags.none),
        ("--grace", flags.grace.is_some()),
        ("--to-file", flags.to_file.is_some()),
        ("--expected-peer-id", flags.expected.is_some()),
        ("--replacing", flags.replacing.is_some()),
        ("--new", flags.new),
        ("--replace", flags.replace),
    ];
    match given.iter().find(|(_, set)| *set) {
        Some((flag, _)) => Err(format!("{flag} does not apply to this command")),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]
    use super::*;

    const PEER: &str = "12D3KooWDpJ7As7BWAwRMfu1VU2WCqNjvq387JEYKDBj4kx6nXTN";

    fn parse_str(line: &str) -> Result<Command, String> {
        parse(line.split_whitespace().map(str::to_owned))
    }

    fn admin_of(line: &str) -> (Admin, bool) {
        match parse_str(line).expect("parses") {
            Command::Admin {
                profile,
                action,
                json,
            } => {
                assert_eq!(profile, "p");
                (action, json)
            }
            other => panic!("an admin command, got {other:?}"),
        }
    }

    fn human() -> EndpointId {
        EndpointId::parse("human").expect("valid")
    }

    #[test]
    fn every_admin_command_parses() {
        assert_eq!(admin_of("--profile p status"), (Admin::Status, false));
        assert_eq!(admin_of("status --json --profile p"), (Admin::Status, true));
        assert_eq!(
            admin_of("--profile p endpoints list --json"),
            (Admin::EndpointsList, true)
        );
        assert_eq!(
            admin_of("--profile p endpoints revoke human"),
            (Admin::Revoke(human()), false)
        );
        assert_eq!(
            admin_of("--profile p endpoints enable human"),
            (Admin::SetEnabled(human(), true), false)
        );
        assert_eq!(
            admin_of("--profile p endpoints disable human"),
            (Admin::SetEnabled(human(), false), false)
        );
        assert_eq!(
            admin_of("--profile p endpoints default human"),
            (Admin::SetDefault(Some(human())), false)
        );
        assert_eq!(
            admin_of("--profile p endpoints default --none"),
            (Admin::SetDefault(None), false)
        );
        assert_eq!(
            admin_of("--profile p shutdown"),
            (Admin::Shutdown(None), false)
        );
        assert_eq!(
            admin_of("--profile p shutdown --grace 250"),
            (Admin::Shutdown(Some(Duration::from_millis(250))), false)
        );
    }

    #[test]
    fn every_identity_command_parses() {
        let peer = TransportIdentity::parse(PEER).expect("valid");
        assert_eq!(
            parse_str("--profile p identity backup").expect("parses"),
            Command::Identity(Identity::Backup {
                profile: "p".into(),
                to_file: None
            })
        );
        assert_eq!(
            parse_str("--profile p identity backup --to-file /x/r.json").expect("parses"),
            Command::Identity(Identity::Backup {
                profile: "p".into(),
                to_file: Some("/x/r.json".into())
            })
        );
        assert_eq!(
            parse_str("identity verify").expect("parses"),
            Command::Identity(Identity::Verify { expected: None })
        );
        assert_eq!(
            parse_str(&format!("identity verify --expected-peer-id {PEER}")).expect("parses"),
            Command::Identity(Identity::Verify {
                expected: Some(peer.clone())
            })
        );
        assert_eq!(
            parse_str("--profile p identity restore --new").expect("parses"),
            Command::Identity(Identity::Restore {
                profile: "p".into(),
                mode: RestoreMode::New,
                expected: None
            })
        );
        assert_eq!(
            parse_str(&format!(
                "--profile p identity restore --replace --replacing {PEER}"
            ))
            .expect("parses"),
            Command::Identity(Identity::Restore {
                profile: "p".into(),
                mode: RestoreMode::Replace { replacing: peer },
                expected: None
            })
        );
    }

    #[test]
    fn help_wins_anywhere() {
        assert_eq!(parse_str("--profile p status --help"), Ok(Command::Help));
        assert_eq!(parse_str("-h"), Ok(Command::Help));
    }

    #[test]
    fn what_a_command_does_not_take_is_a_usage_error() {
        for line in [
            "",
            "status",
            "--profile p",
            "--profile p frobnicate",
            "--profile p endpoints",
            "--profile p endpoints revoke",
            "--profile p endpoints revoke not/an/id",
            "--profile p endpoints default",
            "--profile p endpoints default human --none",
            "--profile p endpoints revoke human --json",
            "--profile p shutdown --json",
            "--profile p shutdown --grace soon",
            "--profile p status --grace 5",
            "--profile p --profile q status",
            "--profile p status --frob",
            "identity",
            "identity backup",
            "--profile p identity verify",
            "--profile p identity backup --json",
            "--profile p identity backup --expected-peer-id x",
            "identity verify --expected-peer-id not-a-peer",
            "--profile p identity restore",
            "--profile p identity restore --new --replace",
            "--profile p identity restore --replace",
            "--profile p identity restore --new --replacing x",
            "--profile p identity verify --to-file f",
        ] {
            assert!(parse_str(line).is_err(), "{line:?} should be refused");
        }
    }

    /// No flag of the grammar takes the phrase, and a phrase put in any
    /// place -- as words, or as one argument to any flag or endpoint -- is
    /// refused WITHOUT being repeated: a refusal is printed to stderr.
    #[test]
    fn a_phrase_on_the_command_line_is_refused_and_never_repeated() {
        let phrase = "abandon abandon abandon abandon abandon abandon abandon abandon \
                      abandon abandon abandon abandon abandon abandon abandon abandon \
                      abandon abandon abandon abandon abandon abandon abandon art";
        let refusal = |args: Vec<String>| match parse(args.clone()) {
            Err(message) => {
                assert!(!message.contains("abandon"), "{args:?} repeated: {message}");
            }
            Ok(command) => panic!("{args:?} parsed as {command:?}"),
        };
        let words =
            |line: &str| -> Vec<String> { line.split_whitespace().map(str::to_owned).collect() };
        refusal(words(&format!(
            "--profile p identity restore --new {phrase}"
        )));
        refusal(words(&format!("identity verify {phrase}")));
        refusal(words(&format!("--profile p status {phrase}")));
        refusal(words(&format!("--profile p -{phrase}")));
        for (head, tail) in [
            ("identity verify --expected-peer-id", ""),
            ("--profile p identity restore --replace --replacing", ""),
            ("--profile p identity restore --new --expected-peer-id", ""),
            ("--profile p endpoints revoke", ""),
            ("--profile p endpoints default", ""),
            ("--profile p shutdown --grace", ""),
        ] {
            let mut args = words(head);
            args.push(phrase.to_owned());
            args.extend(words(tail));
            refusal(args);
        }
    }
}
