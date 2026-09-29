// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! Parsing as strict as the schemas, at every depth (#147 review, F2).
//!
//! Two serde behaviours make a derived type more permissive than the
//! JSON Schema it mirrors, and neither is switched off by
//! `deny_unknown_fields`:
//!
//! - `serde_json` hands a derived STRUCT a JSON array as a sequence, and
//!   the derive fills defaulted fields when the elements run out, so
//!   `"result": []` read as an empty result and `[2, 0]` as a version,
//!   where every schema here says `type: object`;
//! - it hands a derived unit ENUM an object `{"Timeout": null}` as the
//!   variant `Timeout`, where the schema says `enum` of strings.
//!
//! Fixing that per field would leave the next field to be missed, and the
//! types borrowed from `transport-api` out of reach. So every parse in
//! this crate goes through [`from_str`], whose deserializer asks for a
//! MAP wherever a struct is read and a STRING wherever an enum is, at
//! every depth, and passes everything else through untouched.

use serde::de::{
    self, DeserializeSeed, Deserializer, EnumAccess, IntoDeserializer, MapAccess, SeqAccess,
    Visitor,
};

/// Parse `text` as `T`, a struct only from an object and an enum only
/// from a string, at every depth.
///
/// # Errors
/// Whatever `serde_json` or `T` refuses, and the two shapes above.
pub(crate) fn from_str<'a, T: de::Deserialize<'a>>(text: &'a str) -> serde_json::Result<T> {
    let mut json = serde_json::Deserializer::from_str(text);
    let value = T::deserialize(Strict(&mut json))?;
    json.end()?;
    Ok(value)
}

/// The deserializer: `D` with struct and enum reads narrowed.
struct Strict<D>(D);

/// A visitor whose nested deserializers are [`Strict`] too.
struct Wrap<V>(V);

/// A seed that deserializes through [`Strict`].
struct Seed<S>(S);

struct Seq<A>(A);

struct Map<A>(A);

macro_rules! forward_deserialize {
    ($($method:ident)*) => {$(
        fn $method<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
            self.0.$method(Wrap(visitor))
        }
    )*};
}

impl<'de, D: Deserializer<'de>> Deserializer<'de> for Strict<D> {
    type Error = D::Error;

    forward_deserialize! {
        deserialize_any deserialize_bool deserialize_i8 deserialize_i16 deserialize_i32
        deserialize_i64 deserialize_i128 deserialize_u8 deserialize_u16 deserialize_u32
        deserialize_u64 deserialize_u128 deserialize_f32 deserialize_f64 deserialize_char
        deserialize_str deserialize_string deserialize_bytes deserialize_byte_buf
        deserialize_option deserialize_unit deserialize_seq deserialize_map
        deserialize_identifier deserialize_ignored_any
    }

    fn deserialize_unit_struct<V: Visitor<'de>>(
        self,
        name: &'static str,
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        self.0.deserialize_unit_struct(name, Wrap(visitor))
    }

    // Also how `serde_json::value::RawValue` asks for its bytes: the name
    // must reach `serde_json` unchanged.
    fn deserialize_newtype_struct<V: Visitor<'de>>(
        self,
        name: &'static str,
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        self.0.deserialize_newtype_struct(name, Wrap(visitor))
    }

    fn deserialize_tuple<V: Visitor<'de>>(
        self,
        len: usize,
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        self.0.deserialize_tuple(len, Wrap(visitor))
    }

    fn deserialize_tuple_struct<V: Visitor<'de>>(
        self,
        name: &'static str,
        len: usize,
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        self.0.deserialize_tuple_struct(name, len, Wrap(visitor))
    }

    /// A struct is read from an object and nothing else.
    fn deserialize_struct<V: Visitor<'de>>(
        self,
        _: &'static str,
        _: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        self.0.deserialize_map(Wrap(visitor))
    }

    /// Every enum this crate reads is unit-only and spelled as a string:
    /// the variant is read as one and handed over as one.
    fn deserialize_enum<V: Visitor<'de>>(
        self,
        _: &'static str,
        _: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        let name = <String as de::Deserialize>::deserialize(Strict(self.0))?;
        visitor.visit_enum(IntoDeserializer::<Self::Error>::into_deserializer(name))
    }

    fn is_human_readable(&self) -> bool {
        self.0.is_human_readable()
    }
}

macro_rules! forward_visit {
    ($($method:ident: $ty:ty)*) => {$(
        fn $method<E: de::Error>(self, v: $ty) -> Result<Self::Value, E> {
            self.0.$method(v)
        }
    )*};
}

impl<'de, V: Visitor<'de>> Visitor<'de> for Wrap<V> {
    type Value = V::Value;

    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.expecting(f)
    }

    forward_visit! {
        visit_bool: bool visit_i8: i8 visit_i16: i16 visit_i32: i32 visit_i64: i64
        visit_i128: i128 visit_u8: u8 visit_u16: u16 visit_u32: u32 visit_u64: u64
        visit_u128: u128 visit_f32: f32 visit_f64: f64 visit_char: char
        visit_str: &str visit_borrowed_str: &'de str visit_string: String
        visit_bytes: &[u8] visit_borrowed_bytes: &'de [u8] visit_byte_buf: Vec<u8>
    }

    fn visit_none<E: de::Error>(self) -> Result<Self::Value, E> {
        self.0.visit_none()
    }

    fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
        self.0.visit_unit()
    }

    fn visit_some<D: Deserializer<'de>>(self, d: D) -> Result<Self::Value, D::Error> {
        self.0.visit_some(Strict(d))
    }

    fn visit_newtype_struct<D: Deserializer<'de>>(self, d: D) -> Result<Self::Value, D::Error> {
        self.0.visit_newtype_struct(Strict(d))
    }

    fn visit_seq<A: SeqAccess<'de>>(self, seq: A) -> Result<Self::Value, A::Error> {
        self.0.visit_seq(Seq(seq))
    }

    fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Self::Value, A::Error> {
        self.0.visit_map(Map(map))
    }

    fn visit_enum<A: EnumAccess<'de>>(self, data: A) -> Result<Self::Value, A::Error> {
        self.0.visit_enum(data)
    }
}

impl<'de, S: DeserializeSeed<'de>> DeserializeSeed<'de> for Seed<S> {
    type Value = S::Value;

    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<Self::Value, D::Error> {
        self.0.deserialize(Strict(d))
    }
}

impl<'de, A: SeqAccess<'de>> SeqAccess<'de> for Seq<A> {
    type Error = A::Error;

    fn next_element_seed<T: DeserializeSeed<'de>>(
        &mut self,
        seed: T,
    ) -> Result<Option<T::Value>, Self::Error> {
        self.0.next_element_seed(Seed(seed))
    }

    fn size_hint(&self) -> Option<usize> {
        self.0.size_hint()
    }
}

impl<'de, A: MapAccess<'de>> MapAccess<'de> for Map<A> {
    type Error = A::Error;

    fn next_key_seed<K: DeserializeSeed<'de>>(
        &mut self,
        seed: K,
    ) -> Result<Option<K::Value>, Self::Error> {
        self.0.next_key_seed(Seed(seed))
    }

    fn next_value_seed<T: DeserializeSeed<'de>>(
        &mut self,
        seed: T,
    ) -> Result<T::Value, Self::Error> {
        self.0.next_value_seed(Seed(seed))
    }

    fn size_hint(&self) -> Option<usize> {
        self.0.size_hint()
    }
}

#[cfg(test)]
mod tests {
    use super::from_str;
    use serde::Deserialize;

    #[derive(Debug, PartialEq, Deserialize)]
    struct Pair {
        a: u8,
        #[serde(default)]
        b: u8,
    }

    #[derive(Debug, PartialEq, Deserialize)]
    enum Code {
        #[serde(rename = "timeout")]
        Timeout,
    }

    /// The mechanism, with plain `serde_json` as the control that shows it
    /// is real: the same bytes the strict reader refuses, the lax one
    /// accepts.
    #[test]
    fn a_struct_needs_an_object_and_an_enum_a_string_at_every_depth() {
        assert_eq!(
            from_str::<Pair>(r#"{"a":1}"#).ok(),
            Some(Pair { a: 1, b: 0 })
        );
        assert!(
            serde_json::from_str::<Pair>("[1]").is_ok(),
            "the lax control"
        );
        assert!(from_str::<Pair>("[1]").is_err());
        assert!(
            serde_json::from_str::<Vec<Pair>>("[[1]]").is_ok(),
            "nested, lax"
        );
        assert!(from_str::<Vec<Pair>>("[[1]]").is_err(), "nested, strict");
        assert!(from_str::<Option<Pair>>("[1]").is_err(), "inside an Option");

        assert_eq!(from_str::<Code>(r#""timeout""#).ok(), Some(Code::Timeout));
        assert!(
            serde_json::from_str::<Code>(r#"{"timeout":null}"#).is_ok(),
            "the lax control"
        );
        assert!(from_str::<Code>(r#"{"timeout":null}"#).is_err());
        assert!(
            from_str::<Vec<Code>>(r#"[{"timeout":null}]"#).is_err(),
            "nested"
        );
    }

    #[test]
    fn a_raw_value_still_keeps_its_bytes() {
        #[derive(Deserialize)]
        struct Carrier {
            raw: Box<serde_json::value::RawValue>,
        }
        let carrier: Carrier = from_str(r#"{"raw":{"z":1,"a":[2]}}"#).expect("parses");
        assert_eq!(carrier.raw.get(), r#"{"z":1,"a":[2]}"#);
    }

    #[test]
    fn trailing_bytes_are_refused() {
        assert!(from_str::<Pair>(r#"{"a":1} x"#).is_err());
    }
}
