// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! IPC version negotiation (`LOCAL-IPC.md` §Version negotiation and
//! phases; plan §16 (4)).
//!
//! `hello.ipc_version.major` admits ANY positive integer (hello 1.1.0), so
//! an unsupported major is a well-formed hello that is answered, not a
//! malformed one that is refused: the server says what it speaks
//! ([`supported`]) and closes. For the major it speaks it selects
//! `min(client minor, server minor)`. Minors are additive only, so the
//! selected minor is what gates every later method, event and feature.

use serde::{Deserialize, Serialize};

/// The IPC major version this crate implements.
pub const IPC_MAJOR: u64 = 2;

/// The highest IPC minor this build speaks: the first production build
/// spoke 2.0, Stage 15's R1 brought 2.1 (`peer.path_changed`),
/// `admin.peers.list` brought 2.2 (A 2026-10-06), and the persisted
/// trust row brought 2.3 (ADR-0017 A 2026-10-07).
pub const IPC_MAX_MINOR: u64 = 3;

/// A version pair as it crosses the wire.
///
/// Both halves are `u64` and deserialize SATURATING: the schemas bound a
/// major below by 1 and a minor by 0 and above by nothing, so a legal
/// hello may carry a number no fixed-width field holds. Refusing it as
/// malformed would answer a well-formed hello with the wrong error, and
/// would refuse a client whose only fault is a large minor the server
/// would have lowered anyway. Every value past `u64::MAX` is unsupported
/// as a major and larger than any minor this build speaks, so saturating
/// loses nothing a decision reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IpcVersion {
    /// The major, `>= 1` on the wire.
    #[serde(deserialize_with = "positive_integer")]
    pub major: u64,
    /// The minor, `>= 0` on the wire.
    #[serde(deserialize_with = "non_negative_integer")]
    pub minor: u64,
}

/// The versions this build speaks, as `close{VersionIncompatible}` lists
/// them.
#[must_use]
pub const fn supported() -> [IpcVersion; 1] {
    [IpcVersion {
        major: IPC_MAJOR,
        minor: IPC_MAX_MINOR,
    }]
}

/// A proposed major this build does not speak.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnsupportedMajor(pub u64);

/// Select the version a connection speaks from the client's proposal.
///
/// # Errors
/// [`UnsupportedMajor`] for any major other than [`IPC_MAJOR`]; the
/// server answers it `close{VersionIncompatible, supported}`.
pub fn negotiate(proposed: IpcVersion) -> Result<IpcVersion, UnsupportedMajor> {
    if proposed.major != IPC_MAJOR {
        return Err(UnsupportedMajor(proposed.major));
    }
    Ok(IpcVersion {
        major: IPC_MAJOR,
        minor: proposed.minor.min(IPC_MAX_MINOR),
    })
}

fn positive_integer<'de, D: serde::Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
    let value = d.deserialize_any(WideInteger)?;
    if value == 0 {
        return Err(serde::de::Error::custom("a major is at least 1"));
    }
    Ok(value)
}

fn non_negative_integer<'de, D: serde::Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
    d.deserialize_any(WideInteger)
}

/// A JSON integer of any size, saturated at `u64::MAX`.
///
/// `serde_json` hands a number past `u64::MAX` to `visit_f64`, and JSON
/// Schema's `integer` counts `2.0` as an integer too, so a float is
/// accepted when it has no fractional part. A negative, a fraction, a
/// non-finite value and every non-number are refused.
struct WideInteger;

impl serde::de::Visitor<'_> for WideInteger {
    type Value = u64;

    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("a non-negative integer")
    }

    fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<u64, E> {
        Ok(v)
    }

    fn visit_i64<E: serde::de::Error>(self, v: i64) -> Result<u64, E> {
        u64::try_from(v).map_err(|_| E::custom("a version number is never negative"))
    }

    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "the value is finite, integral and inside 0..2^64 when cast"
    )]
    fn visit_f64<E: serde::de::Error>(self, v: f64) -> Result<u64, E> {
        // 2^64 exactly: the first float no u64 holds.
        const PAST_U64: f64 = 18_446_744_073_709_551_616.0;
        if !v.is_finite() || v < 0.0 || v.fract() != 0.0 {
            return Err(E::custom("a version number is a non-negative integer"));
        }
        if v >= PAST_U64 {
            return Ok(u64::MAX);
        }
        Ok(v as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(json: &str) -> Result<IpcVersion, serde_json::Error> {
        serde_json::from_str(json)
    }

    #[test]
    fn the_server_lowers_the_minor_and_keeps_its_major() {
        // min(client, server): a 2.0 client is answered 2.0, and anything
        // above this build's minor is lowered to it.
        for (proposed, selected) in [
            (0, 0),
            (1, 1),
            (2, 2),
            (3, 3),
            (4, 3),
            (u64::MAX, IPC_MAX_MINOR),
        ] {
            assert_eq!(
                negotiate(IpcVersion {
                    major: 2,
                    minor: proposed
                }),
                Ok(IpcVersion {
                    major: 2,
                    minor: selected
                })
            );
        }
    }

    #[test]
    fn every_other_major_is_unsupported_and_answered() {
        for major in [1, 3, u64::MAX] {
            assert_eq!(
                negotiate(IpcVersion { major, minor: 0 }),
                Err(UnsupportedMajor(major))
            );
        }
        assert_eq!(
            supported(),
            [IpcVersion {
                major: IPC_MAJOR,
                minor: IPC_MAX_MINOR
            }]
        );
    }

    /// A major or minor past `u32` -- and past `u64` -- is a well-formed
    /// version; a u32 field refused it as malformed (#143's supply
    /// review).
    #[test]
    fn a_number_no_fixed_width_holds_still_parses() {
        let big = parse(r#"{"major": 4294967296, "minor": 4294967296}"#).expect("u64");
        assert_eq!((big.major, big.minor), (4_294_967_296, 4_294_967_296));
        let huge = parse(r#"{"major": 1e30, "minor": 100000000000000000000000}"#)
            .expect("past u64 saturates");
        assert_eq!((huge.major, huge.minor), (u64::MAX, u64::MAX));
        assert!(negotiate(huge).is_err(), "and is unsupported");
        let integral = parse(r#"{"major": 2.0, "minor": 0}"#).expect("2.0 is an integer");
        assert_eq!(integral.major, 2);
    }

    #[test]
    fn a_major_below_one_and_a_non_integer_are_malformed() {
        for json in [
            r#"{"major": 0, "minor": 0}"#,
            r#"{"major": -1, "minor": 0}"#,
            r#"{"major": 2, "minor": -1}"#,
            r#"{"major": 2.5, "minor": 0}"#,
            r#"{"major": "2", "minor": 0}"#,
            r#"{"major": 2}"#,
            r#"{"major": 2, "minor": 0, "patch": 0}"#,
        ] {
            assert!(parse(json).is_err(), "{json}");
        }
    }
}
