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
    /// An object named the same member twice.
    ///
    /// JSON permits it; every parser resolves it differently, so no
    /// document containing one has a single meaning.
    DuplicateMember {
        /// The member name that repeated.
        name: String,
    },
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
            Self::DuplicateMember { name } => {
                write!(f, "the member '{name}' appears more than once")
            }
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

/// A JSON value that refuses an object naming the same member twice.
///
/// `serde_json::Value` is a map, so it silently keeps the LAST of a
/// repeated pair. RFC 8259 permits the duplicate and says nothing about
/// which one wins, and implementations genuinely differ — first, last, or
/// reject. A document that says `"text"` twice therefore has no single
/// meaning, and a sender who controls both copies chooses which meaning
/// each receiver sees: the one this parser shows a person, and a
/// different one for anything that logs, filters, or bridges the same
/// bytes with a different library.
///
/// This is the same argument the explicit-null handling below already
/// makes — do not be more permissive than a schema-driven implementation
/// — reached one step earlier. Rejection is the only answer that leaves
/// every reader agreeing.
///
/// Rejection is RECURSIVE, and deliberately reaches inside members this
/// version does not model. Unknown members are ignored here for forward
/// compatibility, which means a later version WILL read them; an
/// ambiguity parked inside one is ambiguous for that reader, and this is
/// the only pass over those bytes that could have refused it.
struct StrictValue(serde_json::Value);

impl<'de> serde::Deserialize<'de> for StrictValue {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_any(StrictVisitor)
    }
}

struct StrictVisitor;

impl<'de> serde::de::Visitor<'de> for StrictVisitor {
    type Value = StrictValue;

    fn expecting(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("any JSON value whose objects have unique member names")
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E> {
        Ok(StrictValue(serde_json::Value::Null))
    }

    fn visit_none<E>(self) -> Result<Self::Value, E> {
        Ok(StrictValue(serde_json::Value::Null))
    }

    fn visit_bool<E>(self, v: bool) -> Result<Self::Value, E> {
        Ok(StrictValue(serde_json::Value::Bool(v)))
    }

    fn visit_i64<E>(self, v: i64) -> Result<Self::Value, E> {
        Ok(StrictValue(serde_json::Value::from(v)))
    }

    fn visit_u64<E>(self, v: u64) -> Result<Self::Value, E> {
        Ok(StrictValue(serde_json::Value::from(v)))
    }

    fn visit_f64<E>(self, v: f64) -> Result<Self::Value, E> {
        Ok(StrictValue(serde_json::Value::from(v)))
    }

    fn visit_str<E>(self, v: &str) -> Result<Self::Value, E> {
        Ok(StrictValue(serde_json::Value::String(v.to_owned())))
    }

    fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
    where
        A: serde::de::SeqAccess<'de>,
    {
        let mut items = Vec::new();
        while let Some(StrictValue(item)) = seq.next_element()? {
            items.push(item);
        }
        Ok(StrictValue(serde_json::Value::Array(items)))
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: serde::de::MapAccess<'de>,
    {
        let mut out = serde_json::Map::new();
        while let Some(key) = map.next_key::<String>()? {
            let StrictValue(value) = map.next_value()?;
            // INSERT AND CHECK, not check-then-insert: the returned
            // Option IS the duplicate, so there is no window where the
            // two can disagree.
            if out.insert(key.clone(), value).is_some() {
                return Err(serde::de::Error::custom(format!(
                    "{DUPLICATE_MEMBER_PREFIX}{key}"
                )));
            }
        }
        Ok(StrictValue(serde_json::Value::Object(out)))
    }
}

/// Marks a duplicate-member failure inside a `serde_json` error string.
///
/// `serde::de::Error::custom` is the only channel a visitor has, and it
/// yields a `serde_json::Error`; this prefix is how `parse` recovers the
/// specific refusal rather than reporting a generic syntax error. It is
/// internal and never appears in `EnvelopeError`'s own Display.
const DUPLICATE_MEMBER_PREFIX: &str = "interweave-duplicate-member:";

impl HumanChatV2 {
    /// Parse and validate an envelope from its raw JSON text.
    ///
    /// # Errors
    /// Returns [`EnvelopeError`] for malformed JSON, a wrong version or
    /// kind, a non-canonical id, missing text, an out-of-range timestamp,
    /// or a malformed `from_endpoint`.
    pub fn parse(text: &str) -> Result<Self, EnvelopeError> {
        let StrictValue(raw) = serde_json::from_str::<StrictValue>(text).map_err(|e| {
            let detail = e.to_string();
            detail.find(DUPLICATE_MEMBER_PREFIX).map_or(
                EnvelopeError::NotJson {
                    detail: detail.clone(),
                },
                |at| {
                    let rest = &detail[at + DUPLICATE_MEMBER_PREFIX.len()..];
                    // serde_json appends " at line L column C"; the member
                    // name is everything before it.
                    let name = rest.split(" at line ").next().unwrap_or(rest);
                    EnvelopeError::DuplicateMember {
                        name: name.to_owned(),
                    }
                },
            )
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

        // A MISSING property is absence. An explicit `null` is not: the
        // schema permits a string here and does not include null, so
        // accepting it would make this parser more permissive than every
        // schema-driven implementation — the two would disagree about the
        // same document, which is the drift these types exist to prevent.
        let reply_to = match raw.get("reply_to") {
            None => None,
            Some(v) => Some(
                v.as_str()
                    .filter(|s| is_canonical_id(s))
                    .ok_or(EnvelopeError::MalformedId { field: "reply_to" })?
                    .to_owned(),
            ),
        };

        let sent_at_ms = match raw.get("sent_at_ms") {
            None => None,
            Some(v) => {
                let n = v
                    .as_u64()
                    .filter(|n| *n <= MAX_SENT_AT_MS)
                    .ok_or(EnvelopeError::TimestampOutOfRange)?;
                Some(n)
            }
        };

        let from_endpoint = match raw.get("from_endpoint") {
            None => None,
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
    fn an_explicit_null_is_not_absence() {
        // The schema permits a string or integer for these and does not
        // include null, so accepting null would make this parser more
        // permissive than every schema-driven implementation.
        for field in ["reply_to", "sent_at_ms", "from_endpoint"] {
            let json = envelope(&format!(r#","{field}":null"#));
            assert!(
                HumanChatV2::parse(&json).is_err(),
                "an explicit null {field} should be rejected"
            );
        }
        // Omitting them entirely is still absence.
        let e = HumanChatV2::parse(&envelope("")).expect("parses");
        assert!(e.reply_to.is_none());
        assert!(e.sent_at_ms.is_none());
        assert!(e.from_endpoint.is_none());
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

    #[test]
    fn a_repeated_member_is_refused_rather_than_resolved() {
        // The sender controls both copies, so whichever one a parser
        // keeps is the sender's choice per implementation: this one
        // would have shown the LAST, and a logger, filter or bridge
        // reading the same bytes with a different library can be shown
        // the first. No answer that picks a copy is safe; only refusing
        // leaves every reader agreeing.
        let doubled = format!(
            r#"{{"v":2,"kind":"text","app_message_id":"{ID}","text":"hello","text":"pay attention"}}"#
        );
        assert_eq!(
            HumanChatV2::parse(&doubled),
            Err(EnvelopeError::DuplicateMember {
                name: "text".to_owned()
            }),
            "a duplicate member is refused, and names itself"
        );

        // POSITIVE CONTROL: the same document with one `text` is fine, so
        // this is not a parser that refuses everything.
        assert_eq!(
            HumanChatV2::parse(&envelope(""))
                .expect("the single-member form still parses")
                .text,
            "hi"
        );

        // The version and kind gates run AFTER the whole document is
        // read, so a duplicate cannot hide behind them either.
        let bad_version =
            format!(r#"{{"v":9,"kind":"text","app_message_id":"{ID}","text":"a","text":"b"}}"#);
        assert!(
            matches!(
                HumanChatV2::parse(&bad_version),
                Err(EnvelopeError::DuplicateMember { .. })
            ),
            "ambiguity is refused before any field is interpreted"
        );
    }

    #[test]
    fn a_repeated_member_inside_an_unknown_field_is_refused_too() {
        // Unknown members are ignored HERE for forward compatibility,
        // which is exactly why an ambiguity parked inside one matters: a
        // later version reads that field, and this is the only pass over
        // these bytes that could have refused it.
        let nested = format!(
            r#"{{"v":2,"kind":"text","app_message_id":"{ID}","text":"hi","future":{{"x":1,"x":2}}}}"#
        );
        assert_eq!(
            HumanChatV2::parse(&nested),
            Err(EnvelopeError::DuplicateMember {
                name: "x".to_owned()
            }),
            "the refusal reaches inside a member this version does not model"
        );

        // Arrays are walked too — an object inside one is still an
        // object — and the unknown field itself is otherwise ignored.
        let in_array = format!(
            r#"{{"v":2,"kind":"text","app_message_id":"{ID}","text":"hi","future":[{{"y":1,"y":2}}]}}"#
        );
        assert!(matches!(
            HumanChatV2::parse(&in_array),
            Err(EnvelopeError::DuplicateMember { .. })
        ));
        let clean = format!(
            r#"{{"v":2,"kind":"text","app_message_id":"{ID}","text":"hi","future":[{{"y":1}},2,null]}}"#
        );
        assert!(
            HumanChatV2::parse(&clean).is_ok(),
            "an unknown field with no ambiguity in it is still ignored, not rejected"
        );
    }
}
