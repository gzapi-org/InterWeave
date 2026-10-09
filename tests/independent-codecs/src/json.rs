// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton

//! A small JSON reader and compact writer that keeps object keys in the
//! order they were read.
//!
//! Hand-written rather than borrowed because the IPC goldens freeze a body
//! in key order (`ipc-v2-frame-golden.json`'s `canonical_form`), which the
//! workspace's JSON library loses unless `preserve_order` is on — and it is
//! enabled nowhere, deliberately. A number keeps its lexeme, so a body
//! re-encodes byte-equal whatever its numbers look like.
//!
//! The writer's form is the goldens' one: no whitespace, non-ASCII as raw
//! UTF-8, and only `"`, `\` and the control characters escaped — the short
//! forms `\b \f \n \r \t`, anything else below 0x20 as `\u00xx` in lower
//! case. That is the form the vectors were frozen in, not a rule of the
//! wire: LOCAL-IPC.md pins no canonical JSON, so the reader takes any
//! escape the grammar allows.

use core::fmt::Write as _;

use crate::DecodeError;

/// Nesting deeper than this is refused: the bound keeps a hostile body
/// from turning the reader's recursion into a stack overflow.
pub const MAX_DEPTH: usize = 64;

/// One JSON value, objects in read order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    /// `null`.
    Null,
    /// `true` / `false`.
    Bool(bool),
    /// A number, as its lexeme.
    Number(String),
    /// A string, unescaped.
    String(String),
    /// An array.
    Array(Vec<Value>),
    /// An object, members in the order read. A repeated key is refused at
    /// read, so the order is the only thing a `Vec` adds.
    Object(Vec<(String, Value)>),
}

impl Value {
    /// The member `key` of an object.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Object(members) => members.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    /// The string, if this is one.
    #[must_use]
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::String(s) => Some(s),
            _ => None,
        }
    }

    /// The integer, if this number is one that fits `i64`.
    #[must_use]
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Value::Number(n) => n.parse().ok(),
            _ => None,
        }
    }

    /// Compact text, in the goldens' form.
    #[must_use]
    pub fn to_compact(&self) -> String {
        let mut out = String::new();
        write(self, &mut out);
        out
    }
}

/// Read exactly one value, whitespace around it allowed.
///
/// # Errors
/// Anything RFC 8259 does not allow, a repeated object key, a lone
/// surrogate escape, or nesting past [`MAX_DEPTH`].
pub fn parse(text: &str) -> Result<Value, DecodeError> {
    let mut p = Parser {
        b: text.as_bytes(),
        i: 0,
    };
    p.ws();
    let v = p.value(0)?;
    p.ws();
    if p.i != p.b.len() {
        return Err(p.err("text after the value"));
    }
    Ok(v)
}

struct Parser<'a> {
    b: &'a [u8],
    i: usize,
}

impl Parser<'_> {
    fn err(&self, what: &str) -> DecodeError {
        DecodeError(format!("json at byte {}: {what}", self.i))
    }

    fn ws(&mut self) {
        while let Some(b' ' | b'\t' | b'\n' | b'\r') = self.b.get(self.i) {
            self.i += 1;
        }
    }

    fn eat(&mut self, lit: &[u8]) -> bool {
        if self.b[self.i..].starts_with(lit) {
            self.i += lit.len();
            true
        } else {
            false
        }
    }

    fn value(&mut self, depth: usize) -> Result<Value, DecodeError> {
        if depth > MAX_DEPTH {
            return Err(self.err("nested too deep"));
        }
        match self.b.get(self.i) {
            Some(b'{') => self.object(depth),
            Some(b'[') => self.array(depth),
            Some(b'"') => Ok(Value::String(self.string()?)),
            Some(b't') if self.eat(b"true") => Ok(Value::Bool(true)),
            Some(b'f') if self.eat(b"false") => Ok(Value::Bool(false)),
            Some(b'n') if self.eat(b"null") => Ok(Value::Null),
            Some(b'-' | b'0'..=b'9') => self.number(),
            _ => Err(self.err("no value")),
        }
    }

    fn object(&mut self, depth: usize) -> Result<Value, DecodeError> {
        self.i += 1;
        let mut members: Vec<(String, Value)> = Vec::new();
        self.ws();
        if self.eat(b"}") {
            return Ok(Value::Object(members));
        }
        loop {
            self.ws();
            if self.b.get(self.i) != Some(&b'"') {
                return Err(self.err("an object key is not a string"));
            }
            let key = self.string()?;
            if members.iter().any(|(k, _)| *k == key) {
                return Err(self.err(&format!("repeated key {key:?}")));
            }
            self.ws();
            if !self.eat(b":") {
                return Err(self.err("no ':' after a key"));
            }
            self.ws();
            let v = self.value(depth + 1)?;
            members.push((key, v));
            self.ws();
            if self.eat(b",") {
                continue;
            }
            if self.eat(b"}") {
                return Ok(Value::Object(members));
            }
            return Err(self.err("no ',' or '}' after a member"));
        }
    }

    fn array(&mut self, depth: usize) -> Result<Value, DecodeError> {
        self.i += 1;
        let mut items = Vec::new();
        self.ws();
        if self.eat(b"]") {
            return Ok(Value::Array(items));
        }
        loop {
            self.ws();
            items.push(self.value(depth + 1)?);
            self.ws();
            if self.eat(b",") {
                continue;
            }
            if self.eat(b"]") {
                return Ok(Value::Array(items));
            }
            return Err(self.err("no ',' or ']' after an item"));
        }
    }

    fn number(&mut self) -> Result<Value, DecodeError> {
        let start = self.i;
        self.eat(b"-");
        match self.b.get(self.i) {
            Some(b'0') => self.i += 1,
            Some(b'1'..=b'9') => self.digits(),
            _ => return Err(self.err("a number without digits")),
        }
        if self.eat(b".") {
            if !matches!(self.b.get(self.i), Some(b'0'..=b'9')) {
                return Err(self.err("no digit after '.'"));
            }
            self.digits();
        }
        if let Some(b'e' | b'E') = self.b.get(self.i) {
            self.i += 1;
            if let Some(b'+' | b'-') = self.b.get(self.i) {
                self.i += 1;
            }
            if !matches!(self.b.get(self.i), Some(b'0'..=b'9')) {
                return Err(self.err("no digit in the exponent"));
            }
            self.digits();
        }
        // ASCII by construction: every byte taken above is.
        Ok(Value::Number(
            String::from_utf8_lossy(&self.b[start..self.i]).into_owned(),
        ))
    }

    fn digits(&mut self) {
        while let Some(b'0'..=b'9') = self.b.get(self.i) {
            self.i += 1;
        }
    }

    fn hex4(&mut self) -> Result<u32, DecodeError> {
        let s = self
            .b
            .get(self.i..self.i + 4)
            .ok_or_else(|| self.err("a short \\u escape"))?;
        let s = core::str::from_utf8(s).map_err(|_| self.err("a \\u escape is not hex"))?;
        let v = u32::from_str_radix(s, 16).map_err(|_| self.err("a \\u escape is not hex"))?;
        self.i += 4;
        Ok(v)
    }

    fn string(&mut self) -> Result<String, DecodeError> {
        self.i += 1;
        let mut out = String::new();
        loop {
            let start = self.i;
            while let Some(&c) = self.b.get(self.i) {
                if c == b'"' || c == b'\\' || c < 0x20 {
                    break;
                }
                self.i += 1;
            }
            // The input is a &str, so any run between ASCII delimiters is
            // whole UTF-8.
            out.push_str(
                core::str::from_utf8(&self.b[start..self.i]).map_err(|_| self.err("not UTF-8"))?,
            );
            match self.b.get(self.i) {
                Some(b'"') => {
                    self.i += 1;
                    return Ok(out);
                }
                Some(b'\\') => {
                    self.i += 1;
                    let c = *self.b.get(self.i).ok_or_else(|| self.err("a bare '\\'"))?;
                    self.i += 1;
                    match c {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{8}'),
                        b'f' => out.push('\u{c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => {
                            let hi = self.hex4()?;
                            let cp = if (0xD800..0xDC00).contains(&hi) {
                                if !self.eat(b"\\u") {
                                    return Err(self.err("a lone high surrogate"));
                                }
                                let lo = self.hex4()?;
                                if !(0xDC00..0xE000).contains(&lo) {
                                    return Err(self.err("a high surrogate without its low"));
                                }
                                0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00)
                            } else if (0xDC00..0xE000).contains(&hi) {
                                return Err(self.err("a lone low surrogate"));
                            } else {
                                hi
                            };
                            out.push(
                                char::from_u32(cp).ok_or_else(|| self.err("not a scalar value"))?,
                            );
                        }
                        _ => return Err(self.err("an unknown escape")),
                    }
                }
                Some(_) => return Err(self.err("a raw control character in a string")),
                None => return Err(self.err("an unterminated string")),
            }
        }
    }
}

fn write(v: &Value, out: &mut String) {
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(true) => out.push_str("true"),
        Value::Bool(false) => out.push_str("false"),
        Value::Number(n) => out.push_str(n),
        Value::String(s) => write_str(s, out),
        Value::Array(items) => {
            out.push('[');
            for (n, item) in items.iter().enumerate() {
                if n > 0 {
                    out.push(',');
                }
                write(item, out);
            }
            out.push(']');
        }
        Value::Object(members) => {
            out.push('{');
            for (n, (k, item)) in members.iter().enumerate() {
                if n > 0 {
                    out.push(',');
                }
                write_str(k, out);
                out.push(':');
                write(item, out);
            }
            out.push('}');
        }
    }
}

fn write_str(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_key_order_and_reencodes_compact() {
        let text = r#"{"z":1,"a":[true,null,"x"],"m":{"b":-0.5e3}}"#;
        let v = parse(text).unwrap();
        assert_eq!(v.to_compact(), text);
    }

    #[test]
    fn refuses_a_repeated_key() {
        assert!(parse(r#"{"a":1,"a":2}"#).is_err());
        assert!(parse(r#"{"a":1,"b":2}"#).is_ok());
    }

    #[test]
    fn surrogates_pair_or_are_refused() {
        // Every escape is BUILT here, from a backslash character and the hex
        // digits, never written as a literal: an editor or tool that
        // decodes a literal escape pair in source would turn the test into
        // a raw character and the pairing arithmetic would never run.
        let esc = |units: &[&str]| {
            let mut s = String::from('"');
            for u in units {
                s.push(char::from(0x5c));
                s.push('u');
                s.push_str(u);
            }
            s.push('"');
            s
        };
        for (units, want) in [
            (&["d83d", "de00"][..], 0x0001_f600),
            (&["d800", "dc00"][..], 0x0001_0000),
            (&["dbff", "dfff"][..], 0x0010_ffff),
        ] {
            let text = esc(units);
            assert!(text.is_ascii(), "the escape stayed an escape: {text}");
            assert_eq!(
                parse(&text).unwrap(),
                Value::String(char::from_u32(want).unwrap().to_string()),
                "{text}"
            );
        }
        assert!(parse(&esc(&["d83d"])).is_err(), "a lone high surrogate");
        assert!(parse(&esc(&["de00"])).is_err(), "a lone low surrogate");
        assert!(
            parse(&esc(&["d83d", "0041"])).is_err(),
            "a high surrogate before a non-low"
        );
        let mut high_then_text = esc(&["d83d"]);
        high_then_text.insert(high_then_text.len() - 1, 'A');
        assert!(
            parse(&high_then_text).is_err(),
            "a high surrogate without its low"
        );
    }

    #[test]
    fn depth_is_bounded_at_max_depth() {
        let ok = format!("{}{}", "[".repeat(MAX_DEPTH + 1), "]".repeat(MAX_DEPTH + 1));
        let deep = format!("{}{}", "[".repeat(MAX_DEPTH + 2), "]".repeat(MAX_DEPTH + 2));
        assert!(parse(&ok).is_ok());
        assert!(parse(&deep).is_err());
    }

    #[test]
    fn control_characters_escape_as_the_goldens_do() {
        let v = Value::String("a\u{1}\u{1f}\n\"\\é".to_owned());
        assert_eq!(v.to_compact(), "\"a\\u0001\\u001f\\n\\\"\\\\é\"");
        assert_eq!(parse(&v.to_compact()).unwrap(), v);
    }

    #[test]
    fn grammar_edges_are_refused() {
        for bad in [
            "01",
            "1.",
            "-",
            "1e",
            "[1,]",
            "{\"a\"}",
            "\"\u{1}\"",
            "tru",
            "{} x",
        ] {
            assert!(parse(bad).is_err(), "{bad:?} accepted");
        }
    }
}
