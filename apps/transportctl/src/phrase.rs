// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! Reading the recovery secret: from stdin only, never argv, hidden on a
//! terminal (plan §16 (10)).
//!
//! On a terminal, through `rpassword`, which reads stdin itself
//! (`/dev/stdin`) with echo off, its prompt on the stderr already open. Two facts about that crate shape this module:
//! - It clears `ISIG` and handles Ctrl-C itself, by `raise(SIGINT)`
//!   BEFORE it restores the terminal. Under the default disposition that
//!   kills the process with echo still off, so a SIGINT handler is
//!   registered before the read: the raise is caught, the read returns
//!   `Interrupted`, and the terminal is restored. tokio's handler stays
//!   installed for the rest of this short process, the stream dropped or
//!   not -- so registering is the whole of it.
//! - It accumulates the phrase in a buffer zeroed on drop, but one that
//!   grows by push, and freed reallocations are not zeroed. Those
//!   fragments are a limit of the crate. What it returns is held in
//!   [`Zeroizing`] from that moment on.
//!
//! What this crate's own code holds is zeroed: the pipe's buffer, sized
//! for the bound so it never reallocates; a record's words; the joined
//! phrase. What `serde_json` allocates while parsing a record, and the
//! `bip39` crate inside `RecoveryPhrase`, are theirs.
//!
//! Not on a terminal (a pipe, a file), stdin is read here, bounded.

use std::io::{IsTerminal as _, Read as _};

use interweave_profile_identity::{RecoveryPhrase, RecoveryRecord};
use interweave_transport_api::TransportIdentity;
use zeroize::Zeroizing;

use crate::Failure;

/// What a recovery record or a phrase takes on stdin, with room to
/// spare: 24 words of at most 8 letters, or the record carrying them.
/// Anything longer is not one, and is not read into memory.
const MAX_INPUT: u64 = 16 * 1024;

/// A phrase, and the `PeerId` it must restore when something named one.
pub(crate) struct Recovery {
    /// The phrase.
    pub(crate) phrase: RecoveryPhrase,
    /// The `PeerId` it must restore: the record's, or the flag's -- the
    /// two agreeing when both are given.
    pub(crate) expected: TransportIdentity,
}

/// Read the secret from stdin and settle the `PeerId` it must restore.
///
/// # Errors
/// [`Failure::Refused`] for an interrupted or unreadable input, a phrase
/// or record that does not parse, a record and a flag naming different
/// identities, or no identity named at all -- every recovery command
/// checks the phrase against one (IDENTITY-RECOVERY.md).
pub(crate) fn read(flag: Option<TransportIdentity>) -> Result<Recovery, Failure> {
    let text = if std::io::stdin().is_terminal() {
        from_terminal()?
    } else {
        from_pipe()?
    };
    parse(&text, flag)
}

/// Parse what was read: a JSON recovery record, or a bare phrase.
///
/// # Errors
/// As [`read`].
pub(crate) fn parse(text: &str, flag: Option<TransportIdentity>) -> Result<Recovery, Failure> {
    let refused = |what: &str, e: &dyn std::fmt::Display| Failure::Refused(format!("{what}: {e}"));
    let (phrase, named) = if text.trim_start().starts_with('{') {
        // serde's message can quote what it refused, which here is the
        // secret: only the error's class and position are said.
        let mut record: RecoveryRecord = serde_json::from_str(text).map_err(|e| {
            Failure::Refused(format!(
                "the recovery record does not parse ({:?} error at line {}, column {})",
                e.classify(),
                e.line(),
                e.column()
            ))
        })?;
        let checked = record.validate();
        // Taken before a refusal can return, so they are zeroed either way.
        let words = Zeroizing::new(std::mem::take(&mut record.words));
        checked.map_err(|e| refused("the recovery record", &e))?;
        let joined = Zeroizing::new(words.join(" "));
        let phrase =
            RecoveryPhrase::parse(&joined).map_err(|e| refused("the recovery record", &e))?;
        let named = record
            .expected_peer_id
            .map(TransportIdentity::parse)
            .transpose()
            .map_err(|e| refused("the record's expected_peer_id", &e))?;
        (phrase, named)
    } else {
        (
            RecoveryPhrase::parse(text).map_err(|e| refused("the recovery phrase", &e))?,
            None,
        )
    };
    let expected = match (named, flag) {
        (Some(named), Some(flag)) if named != flag => {
            return Err(Failure::Refused(format!(
                "the record names {} and --expected-peer-id {}: refusing both",
                named.as_str(),
                flag.as_str()
            )));
        }
        (Some(peer), _) | (None, Some(peer)) => peer,
        (None, None) => {
            return Err(Failure::Refused(
                "no PeerId to check the phrase against: give a recovery record that names one, \
                 or --expected-peer-id"
                    .to_owned(),
            ));
        }
    };
    Ok(Recovery { phrase, expected })
}

fn from_terminal() -> Result<Zeroizing<String>, Failure> {
    // Registered before the read: rpassword answers Ctrl-C with
    // raise(SIGINT) before it restores the terminal, and this handler is
    // what lets the restore run (the module doc). Dropping the stream
    // below does not uninstall it.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .build()
        .map_err(|e| Failure::Refused(format!("cannot watch for an interrupt: {e}")))?;
    let interrupt = runtime
        .block_on(async {
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())
        })
        .map_err(|e| Failure::Refused(format!("cannot watch for an interrupt: {e}")))?;
    // stdin itself, never the controlling terminal rpassword defaults to:
    // the phrase comes from stdin only (plan §16 (10)), and a terminal
    // stdin that is not the controlling one is read where it points.
    // The prompt through the stderr ALREADY OPEN, never a reopen of
    // `/dev/stderr`: that is a new open file description, which writes
    // over the head of an appended log and cannot open a socket at all.
    let config = rpassword::ConfigBuilder::new()
        .input_file_path("/dev/stdin")
        .output_writer(std::io::stderr())
        .build();
    let read = rpassword::prompt_password_with_config("recovery phrase (hidden): ", config)
        .map(Zeroizing::new);
    drop(interrupt);
    match read {
        Ok(text) => Ok(text),
        Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {
            Err(Failure::Refused("interrupted".to_owned()))
        }
        Err(e) => Err(Failure::Refused(format!("reading the phrase: {e}"))),
    }
}

fn from_pipe() -> Result<Zeroizing<String>, Failure> {
    // Sized for the bound at once: a buffer that grew would free unzeroed
    // copies of what it held.
    let mut bytes = Zeroizing::new(Vec::with_capacity(
        usize::try_from(MAX_INPUT + 1).unwrap_or(usize::MAX),
    ));
    std::io::stdin()
        .lock()
        .take(MAX_INPUT + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| Failure::Refused(format!("reading stdin: {e}")))?;
    debug_assert_eq!(
        bytes.capacity(),
        usize::try_from(MAX_INPUT + 1).unwrap_or(usize::MAX),
        "the buffer never grew past its first allocation"
    );
    if bytes.len() as u64 > MAX_INPUT {
        return Err(Failure::Refused(format!(
            "stdin carries more than {MAX_INPUT} bytes: not a phrase or a recovery record"
        )));
    }
    let text = std::str::from_utf8(&bytes)
        .map_err(|_| Failure::Refused("stdin is not UTF-8".to_owned()))?;
    Ok(Zeroizing::new(text.to_owned()))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]
    use super::*;

    /// IDENTITY-RECOVERY.md's golden fixture: test-only, never a key.
    const GOLDEN_WORDS: &str = "abandon abandon abandon abandon abandon abandon abandon abandon \
         abandon abandon abandon abandon abandon abandon abandon abandon \
         abandon abandon abandon abandon abandon abandon abandon art";
    const GOLDEN_PEER: &str = "12D3KooWDpJ7As7BWAwRMfu1VU2WCqNjvq387JEYKDBj4kx6nXTN";

    fn peer(id: &str) -> TransportIdentity {
        TransportIdentity::parse(id).expect("valid")
    }

    fn other_peer() -> TransportIdentity {
        interweave_profile_identity::ProfileIdentity::generate()
            .transport_identity()
            .expect("a peer id")
    }

    fn record(expected: Option<&str>) -> String {
        let words: Vec<&str> = GOLDEN_WORDS.split_whitespace().collect();
        let mut record = serde_json::json!({
            "format": interweave_profile_identity::FORMAT,
            "identity_algorithm": interweave_profile_identity::ALGORITHM,
            "words": words,
        });
        if let Some(expected) = expected {
            record["expected_peer_id"] = expected.into();
        }
        record.to_string()
    }

    fn refusal(outcome: Result<Recovery, Failure>) -> String {
        match outcome {
            Err(Failure::Refused(why)) => why,
            Err(other) => panic!("a refusal, got {other:?}"),
            Ok(_) => panic!("a refusal, got a recovery"),
        }
    }

    #[test]
    fn a_phrase_takes_its_peer_from_the_flag() {
        let got = parse(&format!("{GOLDEN_WORDS}\n"), Some(peer(GOLDEN_PEER))).expect("parses");
        assert_eq!(got.expected, peer(GOLDEN_PEER));
    }

    #[test]
    fn a_record_names_its_own_peer_and_a_flag_must_agree() {
        let got = parse(&record(Some(GOLDEN_PEER)), None).expect("parses");
        assert_eq!(got.expected, peer(GOLDEN_PEER));
        let got = parse(&record(Some(GOLDEN_PEER)), Some(peer(GOLDEN_PEER))).expect("agrees");
        assert_eq!(got.expected, peer(GOLDEN_PEER));
        let why = refusal(parse(&record(Some(GOLDEN_PEER)), Some(other_peer())));
        assert!(why.contains("refusing both"), "{why}");
    }

    #[test]
    fn nothing_naming_a_peer_is_refused() {
        let why = refusal(parse(GOLDEN_WORDS, None));
        assert!(why.contains("no PeerId"), "{why}");
        let why = refusal(parse(&record(None), None));
        assert!(why.contains("no PeerId"), "{why}");
    }

    /// A refusal never echoes the words it was given.
    #[test]
    fn a_bad_phrase_is_refused_without_repeating_it() {
        let mutated = GOLDEN_WORDS.replace(" art", " zoo");
        let why = refusal(parse(&mutated, Some(peer(GOLDEN_PEER))));
        assert!(!why.contains("abandon"), "{why}");
        let why = refusal(parse("{\"format\": 1}", None));
        assert!(why.contains("the recovery record"), "{why}");
    }
}
