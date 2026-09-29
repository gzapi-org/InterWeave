// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! Envelope fields kept as the bytes that arrived.

use serde::Deserialize;
use serde_json::value::RawValue;

/// An optional OBJECT, kept raw: absent is `None`; `null`, an array, a
/// string or a number is refused, since every schema here types the
/// field `object`.
pub(crate) fn absent_or_object<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<Option<Box<RawValue>>, D::Error> {
    let raw = Box::<RawValue>::deserialize(d)?;
    // A RawValue is valid JSON with no leading whitespace, so its first
    // byte says what it is.
    if !raw.get().starts_with('{') {
        return Err(serde::de::Error::custom("must be an object"));
    }
    Ok(Some(raw))
}
