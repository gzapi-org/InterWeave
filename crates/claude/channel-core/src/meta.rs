// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! A channel notification's `meta`: string values under fixed keys, in
//! `contracts/CHANNEL-EVENT.md`'s table order.
//!
//! The host renders each key as an attribute of the `<channel>` tag, in
//! the order the bridge sent them (SPIKE-001 fact 10), drops a key outside
//! `^[a-zA-Z_][a-zA-Z0-9_]*$` (fact 12), and renders a key named `source`
//! as a second `source` attribute beside its own (fact 11). So the keys
//! are a closed set, [`MetaKey`], and [`ChannelMeta`] serializes them as an
//! ordered map of its own: a `serde_json::Map` would sort them, since the
//! workspace enables `preserve_order` nowhere. The order survives
//! serializing straight to text (`to_string`, `to_writer`) and is LOST
//! through `serde_json::to_value` or `json!`, which build a `Map` first
//! (`the_order_survives_text_and_not_a_value`).
//!
//! The values are built from validated types (a `PeerId`, an `EndpointId`,
//! a `ChannelId`, a `MediaType`, a hex message id), so none can carry a
//! control character today. [`ChannelMeta::set`] refuses one anyway, and
//! anything past [`MAX_META_VALUE_BYTES`]: the host escapes values for
//! XML, and the bridge's own bound stands beside that, not instead of it
//! (CHANNEL-EVENT.md §Sanitization).

use serde::ser::{Serialize, SerializeMap as _, Serializer};

/// A key the bridge may emit: a closed set, so no caller can name one the
/// contract does not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetaKey {
    /// `broadcast` or `direct`.
    DeliveryMode,
    /// The authenticated transport `PeerId`.
    SourcePeer,
    /// Direct only: the remote peer-asserted endpoint.
    SourceEndpoint,
    /// Direct only: this bridge's leased endpoint.
    DestinationEndpoint,
    /// The transport message id.
    MessageId,
    /// RFC3339 UTC.
    ReceivedAt,
    /// Broadcast only: the channel.
    Channel,
    /// The opaque reply token.
    ReplyToken,
    /// `utf8` or `base64url`.
    PayloadEncoding,
    /// The media type, its content-encoding parameter removed.
    ContentType,
}

impl MetaKey {
    /// Every key, in the contract's table order.
    pub const ALL: [Self; 10] = [
        Self::DeliveryMode,
        Self::SourcePeer,
        Self::SourceEndpoint,
        Self::DestinationEndpoint,
        Self::MessageId,
        Self::ReceivedAt,
        Self::Channel,
        Self::ReplyToken,
        Self::PayloadEncoding,
        Self::ContentType,
    ];

    /// The key as the host sees it. None is `source`
    /// (`every_key_matches_the_hosts_grammar_and_none_is_source`).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::DeliveryMode => "delivery_mode",
            Self::SourcePeer => "source_peer",
            Self::SourceEndpoint => "source_endpoint",
            Self::DestinationEndpoint => "destination_endpoint",
            Self::MessageId => "message_id",
            Self::ReceivedAt => "received_at",
            Self::Channel => "channel",
            Self::ReplyToken => "reply_token",
            Self::PayloadEncoding => "payload_encoding",
            Self::ContentType => "content_type",
        }
    }

    /// Its place in [`MetaKey::ALL`]
    /// (`all_is_every_key_once_in_declaration_order`).
    const fn index(self) -> usize {
        self as usize
    }
}

/// The longest value the bridge emits, in bytes. The longest legitimate
/// one is a 128-byte media type; a `PeerId` is at most a few dozen.
pub const MAX_META_VALUE_BYTES: usize = 256;

/// Why a value was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MetaError {
    /// The value is empty: an absent value is an absent key.
    Empty {
        /// The key.
        key: &'static str,
    },
    /// The value is longer than [`MAX_META_VALUE_BYTES`].
    TooLong {
        /// The key.
        key: &'static str,
        /// Its length in bytes.
        len: usize,
    },
    /// The value holds a control character.
    ControlCharacter {
        /// The key.
        key: &'static str,
    },
}

impl core::fmt::Display for MetaError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Empty { key } => write!(f, "meta.{key} is empty"),
            Self::TooLong { key, len } => write!(
                f,
                "meta.{key} is {len} bytes; the bound is {MAX_META_VALUE_BYTES}"
            ),
            Self::ControlCharacter { key } => write!(f, "meta.{key} holds a control character"),
        }
    }
}

impl core::error::Error for MetaError {}

/// A notification's `meta`, its keys in [`MetaKey::ALL`] order whatever
/// order they were set in, each at most once.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ChannelMeta {
    values: [Option<String>; MetaKey::ALL.len()],
}

impl ChannelMeta {
    /// An empty `meta`.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Set `key` to `value`, replacing what it held.
    ///
    /// # Errors
    /// [`MetaError`] for an empty value, one past
    /// [`MAX_META_VALUE_BYTES`], or one holding a control character.
    pub fn set(&mut self, key: MetaKey, value: impl Into<String>) -> Result<(), MetaError> {
        let value = value.into();
        let name = key.as_str();
        if value.is_empty() {
            return Err(MetaError::Empty { key: name });
        }
        if value.len() > MAX_META_VALUE_BYTES {
            return Err(MetaError::TooLong {
                key: name,
                len: value.len(),
            });
        }
        if value.chars().any(char::is_control) {
            return Err(MetaError::ControlCharacter { key: name });
        }
        self.values[key.index()] = Some(value);
        Ok(())
    }

    /// The value under `key`, if set. Tests' only: the server serializes
    /// `meta` and never reads it back.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn get(&self, key: MetaKey) -> Option<&str> {
        self.values[key.index()].as_deref()
    }

    /// The keys set, with their values, in [`MetaKey::ALL`] order.
    pub(crate) fn entries(&self) -> impl Iterator<Item = (&'static str, &str)> {
        MetaKey::ALL
            .iter()
            .zip(&self.values)
            .filter_map(|(key, value)| value.as_deref().map(|v| (key.as_str(), v)))
    }
}

impl Serialize for ChannelMeta {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(None)?;
        for (key, value) in self.entries() {
            map.serialize_entry(key, value)?;
        }
        map.end()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]
    use super::*;

    /// The host's key grammar, `^[a-zA-Z_][a-zA-Z0-9_]*$` (SPIKE-001
    /// fact 12), and no key named `source` (fact 11; CHANNEL-EVENT.md).
    #[test]
    fn every_key_matches_the_hosts_grammar_and_none_is_source() {
        for key in MetaKey::ALL.map(MetaKey::as_str) {
            let mut chars = key.chars();
            let first = chars.next().expect("a key is not empty");
            assert!(
                first.is_ascii_alphabetic() || first == '_',
                "{key}: the first character"
            );
            assert!(
                chars.all(|c| c.is_ascii_alphanumeric() || c == '_'),
                "{key}: the rest"
            );
            assert_ne!(key, "source");
        }
    }

    /// `ALL` lists each key once, in declaration order, which `index`
    /// relies on.
    #[test]
    fn all_is_every_key_once_in_declaration_order() {
        for (i, key) in MetaKey::ALL.iter().enumerate() {
            assert_eq!(key.index(), i, "{key:?}");
        }
    }

    /// Set in reverse, serialized in the table's order: the input is
    /// neither sorted nor in table order, so neither sorting nor
    /// insertion order would pass.
    #[test]
    fn meta_serializes_in_the_tables_order_whatever_the_setting_order() {
        let mut meta = ChannelMeta::new();
        for key in MetaKey::ALL.iter().rev() {
            meta.set(*key, format!("v_{}", key.as_str()))
                .expect("a value");
        }
        let json = serde_json::to_string(&meta).expect("serializes");
        let keys = MetaKey::ALL.map(MetaKey::as_str);
        let positions: Vec<usize> = keys
            .iter()
            .map(|key| json.find(&format!("\"{key}\":")).expect("present"))
            .collect();
        assert!(positions.is_sorted(), "{json}");
        let mut sorted = keys;
        sorted.sort_unstable();
        assert_ne!(sorted, keys, "the control: table order is not sorted");
    }

    /// The order holds for text and is lost through a `Value`: what the
    /// module doc warns a caller about, pinned so it cannot become false
    /// silently in either direction.
    #[test]
    fn the_order_survives_text_and_not_a_value() {
        let mut meta = ChannelMeta::new();
        meta.set(MetaKey::SourcePeer, "p").expect("a value");
        meta.set(MetaKey::DeliveryMode, "direct").expect("a value");
        assert_eq!(
            serde_json::to_string(&meta).expect("text"),
            r#"{"delivery_mode":"direct","source_peer":"p"}"#,
            "text keeps table order"
        );
        let mut reversed = ChannelMeta::new();
        reversed.set(MetaKey::ReplyToken, "t").expect("a value");
        reversed.set(MetaKey::ContentType, "c").expect("a value");
        let value = serde_json::to_value(&reversed).expect("a value");
        let via_value = serde_json::to_string(&value).expect("text");
        assert_eq!(
            via_value, r#"{"content_type":"c","reply_token":"t"}"#,
            "a Value sorts: reply_token precedes content_type in the table"
        );
    }

    /// An unset key is absent, not empty.
    #[test]
    fn an_unset_key_is_absent() {
        let mut meta = ChannelMeta::new();
        meta.set(MetaKey::DeliveryMode, "broadcast")
            .expect("a value");
        let json = serde_json::to_string(&meta).expect("serializes");
        assert_eq!(json, r#"{"delivery_mode":"broadcast"}"#);
    }

    #[test]
    fn a_control_character_an_empty_or_an_overlong_value_is_refused() {
        let mut meta = ChannelMeta::new();
        let control = Err(MetaError::ControlCharacter { key: "channel" });
        assert_eq!(meta.set(MetaKey::Channel, "a\u{0}b"), control);
        assert_eq!(meta.set(MetaKey::Channel, "a\nb"), control);
        assert_eq!(meta.set(MetaKey::Channel, "a\u{85}b"), control, "a C1 too");
        assert_eq!(
            meta.set(MetaKey::Channel, ""),
            Err(MetaError::Empty { key: "channel" })
        );
        assert_eq!(
            meta.set(MetaKey::Channel, "x".repeat(MAX_META_VALUE_BYTES + 1)),
            Err(MetaError::TooLong {
                key: "channel",
                len: MAX_META_VALUE_BYTES + 1
            })
        );
        assert_eq!(meta.get(MetaKey::Channel), None, "nothing refused was kept");
        meta.set(MetaKey::Channel, "x".repeat(MAX_META_VALUE_BYTES))
            .expect("the control: at the bound");
        assert_eq!(
            meta.get(MetaKey::Channel).map(str::len),
            Some(MAX_META_VALUE_BYTES)
        );
    }
}
