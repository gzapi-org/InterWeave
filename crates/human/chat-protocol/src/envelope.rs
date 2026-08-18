// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The HumanChatV2 envelope.
//!
//! Shape and grammar. The markdown **subset** is deliberately not decided
//! here: an out-of-subset construct falls back to plain-text display
//! rather than rejecting the message, so subset conformance is a rendering
//! contract, not an envelope-validity one (ADR-0050). What this module
//! does provide is the pure policy primitives a renderer needs —
//! [`is_allowed_link_scheme`] and the bounds — so every consumer applies
//! one rule rather than its own reading of it.

use interweave_transport_api::EndpointId;
use serde::{Deserialize, Serialize};

/// Maximum block nesting a renderer may honour.
pub const MAX_BLOCK_NESTING: usize = 16;
/// Maximum table rows a renderer may honour.
pub const MAX_TABLE_ROWS: usize = 256;
/// Maximum table columns a renderer may honour.
pub const MAX_TABLE_COLUMNS: usize = 32;
/// The pinned CommonMark version.
pub const COMMONMARK_VERSION: &str = "0.31.2";
/// The pinned GFM version supplying `table` and `strikethrough`.
pub const GFM_VERSION: &str = "0.29-gfm";
/// The highest legal diagnostic timestamp: 9999-12-31T23:59:59.999Z.
pub const MAX_SENT_AT_MS: u64 = 253_402_300_799_999;

/// The only link schemes a renderer may activate.
///
/// Everything else — `javascript:`, `file:`, `data:` and any scheme not
/// listed — renders as inert plain text.
pub const ALLOWED_LINK_SCHEMES: [&str; 2] = ["https", "mailto"];

/// Whether a renderer may make this link destination active.
///
/// An **allowlist**, not a denylist. A denylist has to anticipate every
/// dangerous scheme and is wrong the moment a new one exists; this is
/// wrong only about schemes that are safe, which costs a working link
/// rather than an execution.
///
/// A destination with no scheme at all — a relative reference — is not
/// activatable either: there is no base to resolve it against in a chat
/// message, and guessing one would invent a destination the sender never
/// wrote.
#[must_use]
pub fn is_allowed_link_scheme(destination: &str) -> bool {
    let Some((scheme, _)) = destination.split_once(':') else {
        return false;
    };
    // Schemes are case-insensitive by RFC 3986, so `HTTPS:` is the same
    // scheme — unlike the media-type parameter values, which the contract
    // pins to one spelling.
    ALLOWED_LINK_SCHEMES
        .iter()
        .any(|s| scheme.eq_ignore_ascii_case(s))
}

/// The only v2 message kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MessageKind {
    /// A text message; `text` carries markdown.
    #[serde(rename = "text")]
    Text,
}

/// Why an envelope was rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnvelopeError {
    /// The JSON did not parse.
    NotJson {
        /// The parser's message.
        detail: String,
    },
    /// `v` was not 2.
    UnsupportedVersion,
    /// `kind` was not `text`.
    UnsupportedKind,
    /// An id was not 32 lowercase hex characters.
    MalformedId {
        /// Which field.
        field: &'static str,
    },
    /// `text` was missing.
    ///
    /// Required. Absence and emptiness are different: a text message with
    /// no text is malformed, while an empty string is a legal message.
    MissingText,
    /// `sent_at_ms` was outside the closed interval.
    TimestampOutOfRange,
    /// `from_endpoint` was not a valid EndpointId.
    MalformedFromEndpoint,
}

impl core::fmt::Display for EnvelopeError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::NotJson { detail } => write!(f, "envelope is not JSON: {detail}"),
            Self::UnsupportedVersion => write!(f, "v must be 2"),
            Self::UnsupportedKind => write!(f, "kind must be 'text'"),
            Self::MalformedId { field } => {
                write!(f, "{field} must be 32 lowercase hexadecimal characters")
            }
            Self::MissingText => write!(
                f,
                "text is required; an empty string is legal, absence is not"
            ),
            Self::TimestampOutOfRange => {
                write!(f, "sent_at_ms must be within 0..={MAX_SENT_AT_MS}")
            }
            Self::MalformedFromEndpoint => write!(f, "from_endpoint is not a valid EndpointId"),
        }
    }
}

impl core::error::Error for EnvelopeError {}

/// A validated HumanChatV2 envelope.
///
/// The schema is deliberately OPEN — unknown fields are ignored for
/// forward compatibility within v2 — so this does **not** use
/// `deny_unknown_fields`. Closing it would break the property the version
/// number exists to provide, and is the one place in this repository where
/// openness is the specified behaviour rather than the fallback.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HumanChatV2 {
    /// Always 2.
    pub v: u32,
    /// Always `text`.
    pub kind: MessageKind,
    /// 32 lowercase hex characters; application reply/retention identity.
    pub app_message_id: String,
    /// UTF-8 markdown. May be empty.
    pub text: String,
    /// An earlier message this replies to, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<String>,
    /// Diagnostic timestamp, never authority.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sent_at_ms: Option<u64>,
    /// An unauthenticated display hint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_endpoint: Option<EndpointId>,
}

fn is_canonical_id(value: &str) -> bool {
    value.len() == 32
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

impl HumanChatV2 {
    /// Parse and validate an envelope from its raw JSON text.
    ///
    /// # Errors
    /// Returns [`EnvelopeError`] for malformed JSON, a wrong version or
    /// kind, a non-canonical id, missing text, an out-of-range timestamp,
    /// or a malformed `from_endpoint`.
    pub fn parse(text: &str) -> Result<Self, EnvelopeError> {
        let raw: serde_json::Value =
            serde_json::from_str(text).map_err(|e| EnvelopeError::NotJson {
                detail: e.to_string(),
            })?;

        if raw.get("v").and_then(serde_json::Value::as_u64) != Some(2) {
            return Err(EnvelopeError::UnsupportedVersion);
        }
        if raw.get("kind").and_then(serde_json::Value::as_str) != Some("text") {
            return Err(EnvelopeError::UnsupportedKind);
        }

        let app_message_id = raw
            .get("app_message_id")
            .and_then(serde_json::Value::as_str)
            .filter(|s| is_canonical_id(s))
            .ok_or(EnvelopeError::MalformedId {
                field: "app_message_id",
            })?
            .to_owned();

        // Present-but-empty is legal; absent is not.
        let text_field = raw
            .get("text")
            .and_then(serde_json::Value::as_str)
            .ok_or(EnvelopeError::MissingText)?
            .to_owned();

        let reply_to = match raw.get("reply_to") {
            None | Some(serde_json::Value::Null) => None,
            Some(v) => Some(
                v.as_str()
                    .filter(|s| is_canonical_id(s))
                    .ok_or(EnvelopeError::MalformedId { field: "reply_to" })?
                    .to_owned(),
            ),
        };

        let sent_at_ms = match raw.get("sent_at_ms") {
            None | Some(serde_json::Value::Null) => None,
            Some(v) => {
                let n = v
                    .as_u64()
                    .filter(|n| *n <= MAX_SENT_AT_MS)
                    .ok_or(EnvelopeError::TimestampOutOfRange)?;
                Some(n)
            }
        };

        let from_endpoint = match raw.get("from_endpoint") {
            None | Some(serde_json::Value::Null) => None,
            Some(v) => Some(
                v.as_str()
                    .and_then(|s| EndpointId::parse(s).ok())
                    .ok_or(EnvelopeError::MalformedFromEndpoint)?,
            ),
        };

        Ok(Self {
            v: 2,
            kind: MessageKind::Text,
            app_message_id,
            text: text_field,
            reply_to,
            sent_at_ms,
            from_endpoint,
        })
    }

    /// Whether `reply_to` names a message this store does not hold.
    ///
    /// Always valid regardless: an unknown reference neither rejects the
    /// envelope nor triggers a network lookup. Treating it as a fetchable
    /// pointer would turn a display hint into a remote-triggered request.
    #[must_use]
    pub fn reply_is_resolvable(&self, known: &dyn Fn(&str) -> bool) -> bool {
        self.reply_to.as_deref().is_none_or(known)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "00000000000000000000000000000000";

    fn envelope(extra: &str) -> String {
        format!(r#"{{"v":2,"kind":"text","app_message_id":"{ID}","text":"hi"{extra}}}"#)
    }

    #[test]
    fn a_minimal_envelope_parses() {
        let e = HumanChatV2::parse(&envelope("")).expect("parses");
        assert_eq!(e.app_message_id, ID);
        assert_eq!(e.text, "hi");
        assert!(e.reply_to.is_none());
    }

    #[test]
    fn text_is_required_but_may_be_empty() {
        // Absence and emptiness are different, and only the first is
        // malformed.
        let missing = format!(r#"{{"v":2,"kind":"text","app_message_id":"{ID}"}}"#);
        assert_eq!(
            HumanChatV2::parse(&missing),
            Err(EnvelopeError::MissingText)
        );
        let empty = format!(r#"{{"v":2,"kind":"text","app_message_id":"{ID}","text":""}}"#);
        assert_eq!(HumanChatV2::parse(&empty).expect("parses").text, "");
    }

    #[test]
    fn ids_must_be_canonical_lowercase_hex() {
        for bad in [
            "A".repeat(32),
            "0".repeat(31),
            "0".repeat(33),
            format!("0x{}", "0".repeat(30)),
            "00000000-0000-0000-0000-000000000000".to_owned(),
            "z".repeat(32),
        ] {
            let json = format!(r#"{{"v":2,"kind":"text","app_message_id":"{bad}","text":"hi"}}"#);
            assert!(HumanChatV2::parse(&json).is_err(), "{bad} should not parse");
        }
    }

    #[test]
    fn unknown_fields_are_ignored_because_the_schema_is_open() {
        // The one place openness is the specified behaviour: closing it
        // would break the property the version number provides.
        let e =
            HumanChatV2::parse(&envelope(r#","future_extension":{"anything":1}"#)).expect("parses");
        assert_eq!(e.text, "hi");
    }

    #[test]
    fn the_timestamp_interval_is_closed_at_both_ends() {
        assert!(HumanChatV2::parse(&envelope(r#","sent_at_ms":0"#)).is_ok());
        assert!(
            HumanChatV2::parse(&envelope(&format!(r#","sent_at_ms":{MAX_SENT_AT_MS}"#))).is_ok()
        );
        assert_eq!(
            HumanChatV2::parse(&envelope(&format!(
                r#","sent_at_ms":{}"#,
                MAX_SENT_AT_MS + 1
            ))),
            Err(EnvelopeError::TimestampOutOfRange)
        );
        assert_eq!(
            HumanChatV2::parse(&envelope(r#","sent_at_ms":-1"#)),
            Err(EnvelopeError::TimestampOutOfRange)
        );
    }

    #[test]
    fn an_unresolvable_reply_stays_valid_and_triggers_no_lookup() {
        let json = envelope(&format!(r#","reply_to":"{}""#, "1".repeat(32)));
        let e = HumanChatV2::parse(&json).expect("parses");
        // Valid, and the store simply does not have it.
        assert!(!e.reply_is_resolvable(&|_| false));
        assert!(e.reply_is_resolvable(&|_| true));
    }

    #[test]
    fn a_v1_envelope_is_refused() {
        let json = format!(r#"{{"v":1,"kind":"text","app_message_id":"{ID}","text":"hi"}}"#);
        assert_eq!(
            HumanChatV2::parse(&json),
            Err(EnvelopeError::UnsupportedVersion)
        );
    }

    #[test]
    fn link_schemes_are_allowlisted_not_denylisted() {
        assert!(is_allowed_link_scheme("https://example.invalid/x"));
        assert!(is_allowed_link_scheme("mailto:someone@example.invalid"));
        // Case-insensitive, per RFC 3986.
        assert!(is_allowed_link_scheme("HTTPS://example.invalid"));

        for blocked in [
            "javascript:alert(1)",
            "file:///etc/passwd",
            "data:text/html;base64,PHNjcmlwdD4=",
            "http://example.invalid",
            "vbscript:x",
            "chrome://settings",
        ] {
            assert!(
                !is_allowed_link_scheme(blocked),
                "{blocked} must stay inert"
            );
        }

        // A relative reference has no base to resolve against in a chat
        // message; guessing one would invent a destination.
        assert!(!is_allowed_link_scheme("/relative/path"));
        assert!(!is_allowed_link_scheme("nothing-at-all"));
    }

    #[test]
    fn the_pinned_dialect_and_bounds_are_stated_once() {
        // Consumers read these rather than each carrying its own reading
        // of the contract.
        assert_eq!(COMMONMARK_VERSION, "0.31.2");
        assert_eq!(GFM_VERSION, "0.29-gfm");
        assert_eq!(MAX_BLOCK_NESTING, 16);
        assert_eq!(MAX_TABLE_ROWS, 256);
        assert_eq!(MAX_TABLE_COLUMNS, 32);
    }
}
