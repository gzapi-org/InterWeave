// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The human client's two local identifiers: a stored row and an
//! application message. Here, beside retention, because both the store
//! and the client's types name them and neither may pull the other in
//! (plan §17 P2, architect-cto's ruling of 2026-10-02). Neither names a
//! sender: a row is local, and an application id is the message's own.

/// A row's local identity within one table.
///
/// Local and non-portable. It is not the `app_message_id`, is never sent
/// anywhere, and does not survive the row: a message that is read and
/// later kept gets a new one, because it is genuinely a new row in a
/// different table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RowId(i64);

impl RowId {
    /// The underlying value, for logging and test assertions.
    #[must_use]
    pub const fn get(self) -> i64 {
        self.0
    }

    /// The id a store assigned and has just read back. Public because the
    /// store is another crate (Rust has no friend crates); what holds is
    /// that a fabricated id names no row, and the store refuses it
    /// (`StoreError::NoSuchRow`, the facade's `RowError::NoSuchRow`).
    #[must_use]
    pub const fn from_stored(value: i64) -> Self {
        Self(value)
    }
}

/// A `HumanChatV2` `app_message_id`: 32 lowercase hex characters.
///
/// Validated on construction so a malformed id cannot reach a UNIQUE
/// column and turn into a constraint error at commit time -- by which
/// point the store would already have decided it was healthy.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AppMessageId(String);

/// An application id that is not 32 lowercase hex characters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MalformedAppMessageId {
    /// What was given.
    pub got: String,
}

impl core::fmt::Display for MalformedAppMessageId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "app_message_id must be 32 lowercase hex characters, got {:?}",
            self.got
        )
    }
}

impl core::error::Error for MalformedAppMessageId {}

impl AppMessageId {
    /// Validate and wrap an application message id.
    ///
    /// # Errors
    /// [`MalformedAppMessageId`] for anything that is not exactly 32
    /// lowercase hex characters -- the grammar `HumanChatV2` states,
    /// restated here so a store need not depend on the envelope parser to
    /// hold its own columns valid.
    pub fn parse(value: impl Into<String>) -> Result<Self, MalformedAppMessageId> {
        let value = value.into();
        let canonical = value.len() == 32
            && value
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
        if canonical {
            Ok(Self(value))
        } else {
            Err(MalformedAppMessageId { got: value })
        }
    }

    /// The id as text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_thirty_two_lowercase_hex_characters_are_an_app_id() {
        assert!(AppMessageId::parse("0123456789abcdef0123456789abcdef").is_ok());
        for bad in [
            "0123456789ABCDEF0123456789ABCDEF",
            "0123456789abcdef0123456789abcde",
            "0123456789abcdef0123456789abcdef0",
            "0x23456789abcdef0123456789abcdef",
            "",
        ] {
            assert_eq!(
                AppMessageId::parse(bad),
                Err(MalformedAppMessageId {
                    got: bad.to_owned()
                }),
                "{bad}"
            );
        }
    }
}
