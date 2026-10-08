// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton

//! The IPC v2 frame and its envelope, from `LOCAL-IPC.md` §Framing and
//! §Message classes and the schema `ipc/frame` with the three it refers to
//! (`ipc/hello`, `ipc/hello-response`, `ipc/close`):
//!
//! ```text
//! N:u32be (1..=131072, the prefix outside N) || N bytes of a UTF-8 JSON object
//! ```
//!
//! The ENVELOPE is what this codec holds: which of the ten classes a body
//! is, the top-level members each class requires and allows, and the rules
//! the frame schema states at that level — a response's `ok` against its
//! `result`/`error`, an event's closed `event_type` set, the request id's
//! bounds, the version objects. What rides inside — a request's params, an
//! event's data, the capability and error-code vocabularies — is the
//! method and event catalogue's, which `ipc/request` and `ipc/event` own,
//! and is carried through as JSON untouched.

use crate::DecodeError;
use crate::json::{self, Value};

/// `LOCAL-IPC.md` §Framing: the body ceiling, the prefix outside it.
pub const MAX_BODY_BYTES: usize = 131_072;

/// The ten classes of `ipc/frame` 2.x.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    /// The client's first frame.
    Hello,
    /// Server-only.
    HelloResponse,
    /// Server-only, connection-fatal.
    Close,
    /// `{id, method, params?, deadline_ms?}`.
    Request,
    /// `{id, ok, result? | error?}`.
    Response,
    /// `{id}`.
    Cancel,
    /// `{sequence, event_type, data?}`.
    Event,
    /// `{health, connectivity?}`.
    ServerState,
    /// `{nonce}`.
    Ping,
    /// `{nonce}`.
    Pong,
}

impl Class {
    fn from_type(t: &str) -> Option<Self> {
        Some(match t {
            "hello" => Self::Hello,
            "hello_response" => Self::HelloResponse,
            "close" => Self::Close,
            "request" => Self::Request,
            "response" => Self::Response,
            "cancel" => Self::Cancel,
            "event" => Self::Event,
            "server_state" => Self::ServerState,
            "ping" => Self::Ping,
            "pong" => Self::Pong,
            _ => return None,
        })
    }

    /// (required, optional) top-level members besides `type`.
    fn members(self) -> (&'static [&'static str], &'static [&'static str]) {
        match self {
            Self::Hello => (
                &["ipc_version", "client"],
                &["endpoint", "requested_capabilities", "features"],
            ),
            Self::HelloResponse => (
                &[
                    "ipc_version",
                    "transport_contract_version",
                    "peer",
                    "granted_capabilities",
                ],
                &["endpoint", "endpoint_lease_epoch", "event_queue"],
            ),
            Self::Close => (&["code"], &["message", "supported"]),
            Self::Request => (&["id", "method"], &["params", "deadline_ms"]),
            Self::Response => (&["id", "ok"], &["result", "error"]),
            Self::Cancel => (&["id"], &[]),
            Self::Event => (&["sequence", "event_type"], &["data"]),
            Self::ServerState => (&["health"], &["connectivity"]),
            Self::Ping | Self::Pong => (&["nonce"], &[]),
        }
    }
}

/// `ipc/frame`'s closed event set at 2.1.0.
const EVENT_TYPES: &[&str] = &[
    "message.direct",
    "message.broadcast",
    "endpoint.lease_changed",
    "peer.disconnected",
    "peer.path_changed",
];

/// One body: its class and the object as read, in key order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Envelope {
    /// Which of the ten.
    pub class: Class,
    /// The whole body, `type` included.
    pub body: Value,
}

impl Envelope {
    /// Read a body (the bytes after the prefix) and hold it to the
    /// envelope rules.
    ///
    /// # Errors
    /// Not UTF-8, not a JSON object, an unknown `type`, a member the class
    /// does not allow or lacks, or a rule of the frame schema broken.
    pub fn decode_body(body: &[u8]) -> Result<Self, DecodeError> {
        let text = core::str::from_utf8(body)
            .map_err(|_| DecodeError("the body is not UTF-8".to_owned()))?;
        let value = json::parse(text)?;
        let Value::Object(members) = &value else {
            return Err(DecodeError("the body is not a JSON object".to_owned()));
        };
        let t = value
            .get("type")
            .and_then(Value::as_str)
            .ok_or_else(|| DecodeError("no string `type`".to_owned()))?;
        let class =
            Class::from_type(t).ok_or_else(|| DecodeError(format!("an unknown class {t:?}")))?;
        let (required, optional) = class.members();
        for r in required {
            if value.get(r).is_none() {
                return Err(DecodeError(format!("{t} without `{r}`")));
            }
        }
        for (k, _) in members {
            if k != "type" && !required.contains(&k.as_str()) && !optional.contains(&k.as_str()) {
                return Err(DecodeError(format!(
                    "{t} with a member `{k}` it does not allow"
                )));
            }
        }
        check_class(class, &value)?;
        Ok(Self { class, body: value })
    }

    /// The body as the goldens freeze it: compact, in the order read.
    #[must_use]
    pub fn encode_body(&self) -> Vec<u8> {
        self.body.to_compact().into_bytes()
    }
}

/// Split one frame off the front of `bytes`: `(body, rest)`.
///
/// # Errors
/// A short prefix, a zero length, a length past the ceiling — refused
/// before the body is taken — or a body shorter than declared.
pub fn split_frame(bytes: &[u8]) -> Result<(&[u8], &[u8]), DecodeError> {
    let prefix = bytes
        .get(..4)
        .ok_or_else(|| DecodeError("fewer than 4 prefix bytes".to_owned()))?;
    let n = u32::from_be_bytes([prefix[0], prefix[1], prefix[2], prefix[3]]);
    let n = usize::try_from(n).unwrap_or(usize::MAX);
    if n == 0 {
        return Err(DecodeError("a zero-length frame".to_owned()));
    }
    if n > MAX_BODY_BYTES {
        return Err(DecodeError(format!(
            "a frame of {n} bytes, above {MAX_BODY_BYTES}"
        )));
    }
    let rest = &bytes[4..];
    if rest.len() < n {
        return Err(DecodeError(format!(
            "{n} body bytes declared, {} present",
            rest.len()
        )));
    }
    Ok(rest.split_at(n))
}

/// Decode exactly one frame.
///
/// # Errors
/// As [`split_frame`] and [`Envelope::decode_body`], or bytes after it.
pub fn decode_frame(bytes: &[u8]) -> Result<Envelope, DecodeError> {
    let (body, rest) = split_frame(bytes)?;
    if !rest.is_empty() {
        return Err(DecodeError(format!(
            "{} byte(s) after the frame",
            rest.len()
        )));
    }
    Envelope::decode_body(body)
}

/// Prefix a body.
///
/// # Errors
/// An empty body or one above the ceiling: no frame carries either.
pub fn encode_frame(body: &[u8]) -> Result<Vec<u8>, DecodeError> {
    if body.is_empty() || body.len() > MAX_BODY_BYTES {
        return Err(DecodeError(format!(
            "a body of {} bytes cannot be framed",
            body.len()
        )));
    }
    let mut out = Vec::with_capacity(4 + body.len());
    out.extend_from_slice(&u32::try_from(body.len()).unwrap_or(u32::MAX).to_be_bytes());
    out.extend_from_slice(body);
    Ok(out)
}

fn check_class(class: Class, v: &Value) -> Result<(), DecodeError> {
    match class {
        Class::Hello => {
            version(v.get("ipc_version"), None)?;
            let client = v.get("client");
            object_with(client, &["kind"], &["version"], "client")?;
            bounded_str(client.and_then(|c| c.get("kind")), 1, 64, "client.kind")?;
            if let Some(e) = v.get("endpoint") {
                object_with(Some(e), &["id"], &[], "endpoint")?;
                bounded_str(e.get("id"), 1, 64, "endpoint.id")?;
            }
        }
        Class::HelloResponse => {
            version(v.get("ipc_version"), Some(2))?;
            let tcv = v.get("transport_contract_version").and_then(Value::as_str);
            if !tcv.is_some_and(is_major_minor) {
                return Err(DecodeError(
                    "transport_contract_version is not N.N".to_owned(),
                ));
            }
        }
        Class::Close => {
            bounded_str(v.get("code"), 1, usize::MAX, "code")?;
            if let Some(Value::Array(items)) = v.get("supported") {
                if items.is_empty() || items.len() > 8 {
                    return Err(DecodeError("supported holds 1..8 versions".to_owned()));
                }
                for item in items {
                    version(Some(item), None)?;
                }
            } else if v.get("supported").is_some() {
                return Err(DecodeError("supported is not an array".to_owned()));
            }
        }
        Class::Request => {
            request_id(v)?;
            bounded_str(v.get("method"), 1, usize::MAX, "method")?;
            if let Some(p) = v.get("params")
                && !matches!(p, Value::Object(_))
            {
                return Err(DecodeError("params is not an object".to_owned()));
            }
            if let Some(d) = v.get("deadline_ms") {
                non_negative(d, "deadline_ms")?;
            }
        }
        Class::Response => {
            request_id(v)?;
            match v.get("ok") {
                Some(Value::Bool(false)) => {
                    if v.get("result").is_some() || v.get("error").is_none() {
                        return Err(DecodeError(
                            "ok:false carries error and no result".to_owned(),
                        ));
                    }
                    let e = v.get("error");
                    object_with(e, &["code"], &["message"], "error")?;
                }
                Some(Value::Bool(true)) => {
                    if v.get("error").is_some() {
                        return Err(DecodeError("ok:true carries no error".to_owned()));
                    }
                }
                _ => return Err(DecodeError("ok is not a boolean".to_owned())),
            }
        }
        Class::Cancel => request_id(v)?,
        Class::Event => {
            non_negative(v.get("sequence").unwrap_or(&Value::Null), "sequence")?;
            let t = v
                .get("event_type")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if !EVENT_TYPES.contains(&t) {
                return Err(DecodeError(format!("an unknown event_type {t:?}")));
            }
            if let Some(d) = v.get("data")
                && !matches!(d, Value::Object(_))
            {
                return Err(DecodeError("data is not an object".to_owned()));
            }
        }
        Class::ServerState => {
            let h = v.get("health").and_then(Value::as_str);
            if !matches!(h, Some("healthy" | "degraded" | "unavailable")) {
                return Err(DecodeError("health outside its closed set".to_owned()));
            }
        }
        Class::Ping | Class::Pong => bounded_str(v.get("nonce"), 1, usize::MAX, "nonce")?,
    }
    Ok(())
}

fn request_id(v: &Value) -> Result<(), DecodeError> {
    bounded_str(v.get("id"), 1, 128, "id")
}

/// Bounds in characters (code points), as LOCAL-IPC.md counts them.
fn bounded_str(v: Option<&Value>, min: usize, max: usize, what: &str) -> Result<(), DecodeError> {
    let n = v
        .and_then(Value::as_str)
        .map(|s| s.chars().count())
        .ok_or_else(|| DecodeError(format!("{what} is not a string")))?;
    if n < min || n > max {
        return Err(DecodeError(format!(
            "{what} of {n} characters, outside {min}..={max}"
        )));
    }
    Ok(())
}

fn non_negative(v: &Value, what: &str) -> Result<(), DecodeError> {
    match v {
        Value::Number(n) if !n.starts_with('-') && n.bytes().all(|b| b.is_ascii_digit()) => Ok(()),
        _ => Err(DecodeError(format!("{what} is not a non-negative integer"))),
    }
}

fn version(v: Option<&Value>, major_const: Option<i64>) -> Result<(), DecodeError> {
    object_with(v, &["major", "minor"], &[], "ipc_version")?;
    let major = v.and_then(|o| o.get("major")).unwrap_or(&Value::Null);
    let minor = v.and_then(|o| o.get("minor")).unwrap_or(&Value::Null);
    non_negative(major, "major")?;
    non_negative(minor, "minor")?;
    if major.as_i64() == Some(0) {
        return Err(DecodeError("major 0".to_owned()));
    }
    if let Some(c) = major_const
        && major.as_i64() != Some(c)
    {
        return Err(DecodeError(format!("major other than {c}")));
    }
    Ok(())
}

fn object_with(
    v: Option<&Value>,
    required: &[&str],
    optional: &[&str],
    what: &str,
) -> Result<(), DecodeError> {
    let Some(Value::Object(members)) = v else {
        return Err(DecodeError(format!("{what} is not an object")));
    };
    for r in required {
        if !members.iter().any(|(k, _)| k == r) {
            return Err(DecodeError(format!("{what} without `{r}`")));
        }
    }
    for (k, _) in members {
        if !required.contains(&k.as_str()) && !optional.contains(&k.as_str()) {
            return Err(DecodeError(format!(
                "{what} with a member `{k}` it does not allow"
            )));
        }
    }
    Ok(())
}

fn is_major_minor(s: &str) -> bool {
    let mut parts = s.split('.');
    let ok =
        |p: Option<&str>| p.is_some_and(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()));
    ok(parts.next()) && ok(parts.next()) && parts.next().is_none()
}
