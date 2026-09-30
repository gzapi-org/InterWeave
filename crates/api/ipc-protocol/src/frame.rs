// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The frame body: ONE envelope over the ten mutually exclusive classes of
//! `ipc/frame.schema.json` 2.0.0 (`LOCAL-IPC.md` §Message classes).
//!
//! [`Frame::parse`] reads the `type` first and then the class's own
//! shape, so a body with an unknown class and a body of a known class in
//! the wrong shape are both `ProtocolViolation` -- and a request with an
//! unknown METHOD is not: that is a well-formed request, answered on a
//! connection that stays (`request.rs`).
//!
//! Which side may send which class, and in which phase, is prose plus
//! `tests/ipc-v2` (JSON Schema cannot express order); [`Frame::sender`]
//! states the direction half so both ends read it from one place.

use std::collections::BTreeSet;
use std::num::NonZeroU32;
use std::time::Duration;

use interweave_local_client_api::{AdminCapability, DataCapability, Generation};
use interweave_transport_api::{
    ConnectivitySummary, EndpointId, Health, TransportError, TransportIdentity,
};
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;

use crate::event::EventFrame;
use crate::framing::{FrameError, encode_frame};
use crate::handshake::{HandshakeOutcome, Hello, MAX_REQUESTED, RequestedCapability};
use crate::request::{RequestFrame, RequestId};
use crate::version::{IPC_MAJOR, IpcVersion, UnsupportedMajor, supported};

/// How long a connection has to send its `hello`; expiry is answered
/// `close{Timeout}`. A protocol constant, not a profile value.
pub const HELLO_TIMEOUT: Duration = Duration::from_secs(5);

/// The transport contract this implementation speaks: Model B is
/// transport v2. Distinct from the IPC version.
pub const TRANSPORT_CONTRACT_VERSION: &str = "2.0";

/// The longest `message` a `close` or an error carries, in CHARACTERS
/// (Unicode code points): `ipc/close` and `ipc/frame` say
/// `maxLength: 2048`, and characters are the unit ruled for every string
/// bound (architect-cto, 2026-09-29; `LOCAL-IPC.md` §Framing, #148). A
/// maximal message is at most 8 KiB decoded, and at most
/// 24 KiB written as escapes, well inside the frame ceiling either way.
pub const MAX_MESSAGE_CHARS: usize = 2048;

/// The most versions a `close` lists as supported.
pub const MAX_SUPPORTED_VERSIONS: usize = 8;

/// Which end of a connection sends a frame class.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sender {
    /// `hello`, `request`, `cancel`, `pong`.
    Client,
    /// `hello_response`, `close`, `response`, `event`, `server_state`,
    /// `ping`.
    Server,
}

/// One frame body of any class.
#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum Frame {
    /// The client's first frame.
    Hello(Hello),
    /// The server's answer to it.
    HelloResponse(HelloResponse),
    /// The server's connection-fatal reply.
    Close(Close),
    /// A method call.
    Request(RequestFrame),
    /// Its answer.
    Response(ResponseFrame),
    /// An advisory cancellation.
    Cancel(Cancel),
    /// A pushed event.
    Event(EventFrame),
    /// The server's health push.
    ServerState(ServerState),
    /// A keepalive probe.
    Ping(Ping),
    /// Its echo.
    Pong(Pong),
}

impl Frame {
    /// Parse a frame body.
    ///
    /// # Errors
    /// [`TransportError::ProtocolViolation`] for a body that is not an
    /// object with a known `type`, or not that class's shape.
    pub fn parse(body: &str) -> Result<Self, TransportError> {
        #[derive(Deserialize)]
        struct Class<'a> {
            #[serde(rename = "type", borrow)]
            class: std::borrow::Cow<'a, str>,
        }
        fn shaped<T: serde::de::DeserializeOwned>(body: &str) -> Result<T, TransportError> {
            crate::strict::from_str(body).map_err(|_| TransportError::ProtocolViolation)
        }
        let class: Class<'_> =
            crate::strict::from_str(body).map_err(|_| TransportError::ProtocolViolation)?;
        Ok(match &*class.class {
            "hello" => Self::Hello(shaped(body)?),
            "hello_response" => Self::HelloResponse(shaped(body)?),
            "close" => Self::Close(shaped(body)?),
            "request" => Self::Request(shaped(body)?),
            "response" => Self::Response(shaped(body)?),
            "cancel" => Self::Cancel(shaped(body)?),
            "event" => Self::Event(shaped(body)?),
            "server_state" => Self::ServerState(shaped(body)?),
            "ping" => Self::Ping(shaped(body)?),
            "pong" => Self::Pong(shaped(body)?),
            _ => return Err(TransportError::ProtocolViolation),
        })
    }

    /// The body as compact JSON.
    ///
    /// # Panics
    /// Never: every class serializes.
    #[must_use]
    pub fn to_body(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| unreachable!("a frame serializes"))
    }

    /// The body, length-prefixed for the wire.
    ///
    /// # Errors
    /// [`FrameError::BodyTooLarge`] past the 128 KiB ceiling.
    pub fn encode(&self) -> Result<Vec<u8>, FrameError> {
        encode_frame(&self.to_body())
    }

    /// Which end sends this class.
    #[must_use]
    pub const fn sender(&self) -> Sender {
        match self {
            Self::Hello(_) | Self::Request(_) | Self::Cancel(_) | Self::Pong(_) => Sender::Client,
            Self::HelloResponse(_)
            | Self::Close(_)
            | Self::Response(_)
            | Self::Event(_)
            | Self::ServerState(_)
            | Self::Ping(_) => Sender::Server,
        }
    }
}

macro_rules! tag {
    ($(#[$doc:meta])* $name:ident, $wire:literal) => {
        $(#[$doc])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
        pub enum $name {
            /// The only legal value.
            #[serde(rename = $wire)]
            Tag,
        }
    };
}

tag!(
    /// The literal `"hello_response"`.
    HelloResponseTag,
    "hello_response"
);
tag!(
    /// The literal `"close"`.
    CloseTag,
    "close"
);
tag!(
    /// The literal `"response"`.
    ResponseTag,
    "response"
);
tag!(
    /// The literal `"cancel"`.
    CancelTag,
    "cancel"
);
tag!(
    /// The literal `"server_state"`.
    ServerStateTag,
    "server_state"
);
tag!(
    /// The literal `"ping"`.
    PingTag,
    "ping"
);
tag!(
    /// The literal `"pong"`.
    PongTag,
    "pong"
);

/// The server's answer to a hello: what the connection HAS, never what it
/// asked for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HelloResponse {
    /// Always `"hello_response"`.
    #[serde(rename = "type")]
    pub frame_type: HelloResponseTag,
    /// The selected version. [`HelloResponse::new`] writes [`IPC_MAJOR`]
    /// whatever it is given, and the parser refuses any other major; the
    /// field is public, so a hand-built value is the builder's to keep.
    pub ipc_version: IpcVersion,
    /// [`TRANSPORT_CONTRACT_VERSION`].
    pub transport_contract_version: String,
    /// The profile's identity.
    pub peer: TransportIdentity,
    /// The endpoint actually leased, with its fresh epoch; both or
    /// neither (`ipc/hello-response.schema.json`'s two implications).
    #[serde(flatten)]
    pub lease: Option<GrantedLease>,
    /// The capabilities actually granted.
    pub granted_capabilities: BTreeSet<RequestedCapability>,
}

/// A granted lease: the endpoint, the epoch that names this grant, and
/// the event queue bound that comes with it (hello-response 1.1.0).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GrantedLease {
    /// The endpoint leased.
    pub endpoint: EndpointId,
    /// Fresh for every grant; not a bearer credential.
    pub endpoint_lease_epoch: Generation,
    /// How many events the server holds for this connection before its
    /// overflow rules apply; the client sizes its receive buffer by it.
    pub event_queue: NonZeroU32,
}

impl HelloResponse {
    /// The answer for a hello the server admitted: `version` as
    /// [`crate::negotiate`] selected it, the grants as
    /// [`Hello::evaluate`] and policy decided them.
    #[must_use]
    pub fn new(
        version: IpcVersion,
        peer: TransportIdentity,
        lease: Option<GrantedLease>,
        outcome: &HandshakeOutcome,
    ) -> Self {
        let granted_capabilities = outcome
            .granted_data
            .iter()
            .map(|c| match c {
                DataCapability::Events => RequestedCapability::Events,
                DataCapability::Commands => RequestedCapability::Commands,
                DataCapability::EndpointsQuery => RequestedCapability::EndpointsQuery,
            })
            .chain(outcome.granted_admin.iter().map(|c| match c {
                AdminCapability::Status => RequestedCapability::AdminStatus,
                AdminCapability::Endpoints => RequestedCapability::AdminEndpoints,
                AdminCapability::Shutdown => RequestedCapability::AdminShutdown,
            }))
            .collect();
        Self {
            frame_type: HelloResponseTag::Tag,
            ipc_version: IpcVersion {
                major: IPC_MAJOR,
                minor: version.minor,
            },
            transport_contract_version: TRANSPORT_CONTRACT_VERSION.to_owned(),
            peer,
            lease,
            granted_capabilities,
        }
    }
}

impl<'de> Deserialize<'de> for HelloResponse {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        use serde::de::Error as _;
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            #[serde(rename = "type")]
            frame_type: HelloResponseTag,
            ipc_version: IpcVersion,
            transport_contract_version: String,
            peer: TransportIdentity,
            #[serde(default, deserialize_with = "absent_or")]
            endpoint: Option<EndpointId>,
            #[serde(default, deserialize_with = "absent_or")]
            endpoint_lease_epoch: Option<Generation>,
            #[serde(default, deserialize_with = "absent_or")]
            event_queue: Option<NonZeroU32>,
            granted_capabilities: Vec<RequestedCapability>,
        }
        let wire = Wire::deserialize(d)?;
        if wire.ipc_version.major != IPC_MAJOR {
            return Err(D::Error::custom("a hello_response speaks major 2"));
        }
        if !is_contract_version(&wire.transport_contract_version) {
            return Err(D::Error::custom(
                "transport_contract_version is <digits>.<digits>",
            ));
        }
        let count = wire.granted_capabilities.len();
        let granted_capabilities: BTreeSet<_> = wire.granted_capabilities.into_iter().collect();
        if count > MAX_REQUESTED || granted_capabilities.len() != count {
            return Err(D::Error::custom(
                "granted_capabilities is at most 8 unique entries",
            ));
        }
        let lease = match (wire.endpoint, wire.endpoint_lease_epoch, wire.event_queue) {
            (Some(endpoint), Some(endpoint_lease_epoch), Some(event_queue)) => Some(GrantedLease {
                endpoint,
                endpoint_lease_epoch,
                event_queue,
            }),
            (None, None, None) => None,
            _ => {
                return Err(D::Error::custom(
                    "endpoint, endpoint_lease_epoch and event_queue come together or not at all",
                ));
            }
        };
        Ok(Self {
            frame_type: wire.frame_type,
            ipc_version: wire.ipc_version,
            transport_contract_version: wire.transport_contract_version,
            peer: wire.peer,
            lease,
            granted_capabilities,
        })
    }
}

fn is_contract_version(text: &str) -> bool {
    let mut parts = text.split('.');
    let digits =
        |p: Option<&str>| p.is_some_and(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()));
    digits(parts.next()) && digits(parts.next()) && parts.next().is_none()
}

/// The server's connection-fatal reply; the connection closes after it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Close {
    /// Always `"close"`.
    #[serde(rename = "type")]
    pub frame_type: CloseTag,
    /// Why.
    pub code: TransportError,
    /// Optional detail for a person, never for a program to branch on.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// What the server speaks. [`Close::new`] sets it exactly when the
    /// code is `VersionIncompatible`; a parsed close must carry it with
    /// that code and may carry it with any other, as the schema allows.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supported: Option<Vec<IpcVersion>>,
}

impl Close {
    /// A close for `code`. `VersionIncompatible` goes through
    /// [`Close::version_incompatible`], which says what to speak.
    #[must_use]
    pub fn new(code: TransportError) -> Self {
        Self {
            frame_type: CloseTag::Tag,
            code,
            message: None,
            supported: (code == TransportError::VersionIncompatible).then(|| supported().to_vec()),
        }
    }

    /// The answer to a hello whose major this build does not speak.
    #[must_use]
    pub fn version_incompatible(_: UnsupportedMajor) -> Self {
        Self::new(TransportError::VersionIncompatible)
    }

    /// The same close with a message, cut to [`MAX_MESSAGE_CHARS`]
    /// characters: the message is for a person, and a close must still
    /// be sendable when the detail is long.
    #[must_use]
    pub fn with_message(mut self, message: &str) -> Self {
        self.message = Some(message.chars().take(MAX_MESSAGE_CHARS).collect());
        self
    }
}

impl<'de> Deserialize<'de> for Close {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        use serde::de::Error as _;
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            #[serde(rename = "type")]
            frame_type: CloseTag,
            code: TransportError,
            #[serde(default, deserialize_with = "absent_or_message")]
            message: Option<String>,
            #[serde(default, deserialize_with = "absent_or")]
            supported: Option<Vec<IpcVersion>>,
        }
        let wire = Wire::deserialize(d)?;
        if let Some(versions) = &wire.supported
            && (versions.is_empty() || versions.len() > MAX_SUPPORTED_VERSIONS)
        {
            return Err(D::Error::custom("supported lists 1..=8 versions"));
        }
        if wire.code == TransportError::VersionIncompatible && wire.supported.is_none() {
            return Err(D::Error::custom(
                "a VersionIncompatible close says what is supported",
            ));
        }
        Ok(Self {
            frame_type: wire.frame_type,
            code: wire.code,
            message: wire.message,
            supported: wire.supported,
        })
    }
}

/// A request's answer: `ok` with an optional `result`, or not `ok` with
/// an `error` and no `result`.
#[derive(Debug, Clone, Serialize)]
pub struct ResponseFrame {
    /// Always `"response"`.
    #[serde(rename = "type")]
    pub frame_type: ResponseTag,
    /// The request answered.
    pub id: RequestId,
    /// Whether it succeeded.
    pub ok: bool,
    /// The method's result, as the bytes that arrived.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Box<RawValue>>,
    /// Why it failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ResponseError>,
}

/// A failed response's error.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResponseError {
    /// The stable code a caller branches on.
    pub code: TransportError,
    /// Optional detail for a person.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "absent_or_message"
    )]
    pub message: Option<String>,
}

impl ResponseFrame {
    /// A successful answer carrying `result`.
    ///
    /// # Panics
    /// Never for this crate's result types, which all serialize.
    #[must_use]
    pub fn success<T: Serialize>(id: RequestId, result: &T) -> Self {
        Self {
            frame_type: ResponseTag::Tag,
            id,
            ok: true,
            result: Some(
                serde_json::value::to_raw_value(result)
                    .unwrap_or_else(|_| unreachable!("a result serializes")),
            ),
            error: None,
        }
    }

    /// A failed answer.
    #[must_use]
    pub fn failure(id: RequestId, code: TransportError) -> Self {
        Self {
            frame_type: ResponseTag::Tag,
            id,
            ok: false,
            result: None,
            error: Some(ResponseError {
                code,
                message: None,
            }),
        }
    }

    /// The outcome as the neutral ports state it: the result as `T`, or
    /// the error's code. An absent result reads as `{}`.
    ///
    /// # Errors
    /// The response's code, or [`TransportError::ProtocolViolation`] for a
    /// result that is not `T`'s shape.
    pub fn outcome<T: serde::de::DeserializeOwned>(&self) -> Result<T, TransportError> {
        if let Some(error) = &self.error {
            return Err(error.code);
        }
        let body = self.result.as_deref().map_or("{}", RawValue::get);
        crate::strict::from_str(body).map_err(|_| TransportError::ProtocolViolation)
    }
}

impl<'de> Deserialize<'de> for ResponseFrame {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        use serde::de::Error as _;
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            #[serde(rename = "type")]
            frame_type: ResponseTag,
            id: RequestId,
            ok: bool,
            #[serde(default, deserialize_with = "absent_or")]
            result: Option<Box<RawValue>>,
            #[serde(default, deserialize_with = "absent_or")]
            error: Option<ResponseError>,
        }
        let wire = Wire::deserialize(d)?;
        let shaped = if wire.ok {
            wire.error.is_none()
        } else {
            wire.error.is_some() && wire.result.is_none()
        };
        if !shaped {
            return Err(D::Error::custom(
                "ok carries no error; not ok carries an error and no result",
            ));
        }
        Ok(Self {
            frame_type: wire.frame_type,
            id: wire.id,
            ok: wire.ok,
            result: wire.result,
            error: wire.error,
        })
    }
}

/// An advisory cancellation of a pending request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Cancel {
    /// Always `"cancel"`.
    #[serde(rename = "type")]
    pub frame_type: CancelTag,
    /// The request to cancel.
    pub id: RequestId,
}

impl Cancel {
    /// Cancel `id`.
    #[must_use]
    pub const fn new(id: RequestId) -> Self {
        Self {
            frame_type: CancelTag::Tag,
            id,
        }
    }
}

/// The server's health push.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerState {
    /// Always `"server_state"`.
    #[serde(rename = "type")]
    pub frame_type: ServerStateTag,
    /// The closed health set.
    pub health: Health,
    /// The normalized connectivity summary, when the server sends it.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "absent_or"
    )]
    pub connectivity: Option<ConnectivitySummary>,
}

impl ServerState {
    /// A health push.
    #[must_use]
    pub const fn new(health: Health, connectivity: Option<ConnectivitySummary>) -> Self {
        Self {
            frame_type: ServerStateTag::Tag,
            health,
            connectivity,
        }
    }
}

/// A keepalive nonce: 16..=64 bytes of `[A-Za-z0-9_-]`. Not
/// authentication and not a renewal credential; only an exact echo of the
/// current one satisfies a probe.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct Nonce(String);

impl Nonce {
    /// Wrap a nonce.
    ///
    /// # Errors
    /// [`TransportError::InvalidArgument`] outside the grammar.
    pub fn new(nonce: impl Into<String>) -> Result<Self, TransportError> {
        let nonce = nonce.into();
        let legal = (16..=64).contains(&nonce.len())
            && nonce
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
        if !legal {
            return Err(TransportError::InvalidArgument);
        }
        Ok(Self(nonce))
    }

    /// The nonce's text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for Nonce {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Self::new(String::deserialize(d)?)
            .map_err(|_| serde::de::Error::custom("a nonce is 16..=64 of [A-Za-z0-9_-]"))
    }
}

/// A keepalive probe, server to client; at most one outstanding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Ping {
    /// Always `"ping"`.
    #[serde(rename = "type")]
    pub frame_type: PingTag,
    /// The current nonce.
    pub nonce: Nonce,
}

/// A keepalive echo, client to server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Pong {
    /// Always `"pong"`.
    #[serde(rename = "type")]
    pub frame_type: PongTag,
    /// The nonce, echoed exactly.
    pub nonce: Nonce,
}

impl Ping {
    /// A probe carrying `nonce`.
    #[must_use]
    pub const fn new(nonce: Nonce) -> Self {
        Self {
            frame_type: PingTag::Tag,
            nonce,
        }
    }

    /// The echo that satisfies this probe.
    #[must_use]
    pub fn echo(&self) -> Pong {
        Pong {
            frame_type: PongTag::Tag,
            nonce: self.nonce.clone(),
        }
    }
}

/// A field that may be absent but never `null`.
fn absent_or<'de, D, T>(d: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(d).map(Some)
}

fn absent_or_message<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    let message = String::deserialize(d)?;
    if message.chars().count() > MAX_MESSAGE_CHARS {
        return Err(serde::de::Error::custom(format!(
            "a message is at most {MAX_MESSAGE_CHARS} characters"
        )));
    }
    Ok(Some(message))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const PEER: &str = "12D3KooWDpJ7As7BWAwRMfu1VU2WCqNjvq387JEYKDBj4kx6nXTN";

    fn parse(value: &serde_json::Value) -> Result<Frame, TransportError> {
        Frame::parse(&value.to_string())
    }

    #[test]
    fn a_class_outside_the_ten_or_in_the_wrong_shape_is_a_violation() {
        for value in [
            json!({"type": "goodbye"}),
            json!({"kind": "hello"}),
            json!(["hello"]),
            json!({"type": "ping"}),
            json!({"type": "cancel", "id": "1", "extra": true}),
            json!({"type": "server_state", "health": "unknown-state"}),
        ] {
            assert_eq!(
                parse(&value).err(),
                Some(TransportError::ProtocolViolation),
                "{value}"
            );
        }
    }

    #[test]
    fn an_unknown_method_is_a_well_formed_request() {
        let frame = parse(&json!({"type": "request", "id": "1", "method": "admin.trust.add"}))
            .expect("parses");
        assert!(matches!(frame, Frame::Request(r) if r.method == "admin.trust.add"));
    }

    #[test]
    fn a_lease_is_both_halves_or_neither() {
        let base = || {
            json!({"type": "hello_response", "ipc_version": {"major": 2, "minor": 0},
                   "transport_contract_version": "2.0", "peer": PEER,
                   "granted_capabilities": []})
        };
        assert!(parse(&base()).is_ok());
        let mut half = base();
        half["endpoint"] = json!("human");
        assert!(parse(&half).is_err(), "an endpoint without its epoch");
        let mut other_half = base();
        other_half["endpoint_lease_epoch"] = json!("AAAAAAAAAAAAAAAA");
        assert!(parse(&other_half).is_err(), "an epoch without its endpoint");
        let mut major = base();
        major["ipc_version"] = json!({"major": 3, "minor": 0});
        assert!(parse(&major).is_err(), "a hello_response speaks 2");
        let mut version = base();
        version["transport_contract_version"] = json!("2");
        assert!(parse(&version).is_err());
    }

    #[test]
    fn a_hello_response_speaks_major_2_whatever_it_is_given() {
        let outcome = HandshakeOutcome {
            granted_data: BTreeSet::new(),
            granted_admin: BTreeSet::new(),
            endpoint: None,
        };
        let response = HelloResponse::new(
            IpcVersion { major: 7, minor: 0 },
            TransportIdentity::parse(PEER).expect("peer"),
            None,
            &outcome,
        );
        assert_eq!(
            response.ipc_version,
            IpcVersion {
                major: IPC_MAJOR,
                minor: 0
            }
        );
    }

    #[test]
    fn a_hello_response_grants_what_the_outcome_holds_and_nothing_asked() {
        let outcome = HandshakeOutcome {
            granted_data: [DataCapability::Events].into(),
            granted_admin: BTreeSet::new(),
            endpoint: None,
        };
        let response = HelloResponse::new(
            IpcVersion { major: 2, minor: 0 },
            TransportIdentity::parse(PEER).expect("peer"),
            None,
            &outcome,
        );
        assert_eq!(
            response.granted_capabilities,
            [RequestedCapability::Events].into()
        );
        let body = Frame::HelloResponse(response).to_body();
        assert!(
            !body.contains("endpoint"),
            "no lease, no lease fields: {body}"
        );
    }

    #[test]
    fn version_incompatible_says_what_to_speak_and_nothing_else_must() {
        let close = Close::version_incompatible(UnsupportedMajor(3));
        assert_eq!(close.supported, Some(supported().to_vec()));
        assert_eq!(Close::new(TransportError::Timeout).supported, None);
        assert!(
            parse(&json!({"type": "close", "code": "VersionIncompatible"})).is_err(),
            "the schema requires supported with this code"
        );
        assert!(
            parse(&json!({"type": "close", "code": "VersionIncompatible", "supported": []}))
                .is_err()
        );
    }

    #[test]
    fn a_message_is_bounded_in_characters_both_ways() {
        // 2048 three-byte characters: 6144 bytes, which the schema admits.
        let long = "ა".repeat(MAX_MESSAGE_CHARS);
        assert!(parse(&json!({"type": "close", "code": "Timeout", "message": long})).is_ok());
        let longer = "ა".repeat(MAX_MESSAGE_CHARS + 1);
        assert!(parse(&json!({"type": "close", "code": "Timeout", "message": longer})).is_err());
        // Sent: cut to the same 2048 characters, whatever their width.
        let cut = Close::new(TransportError::Timeout).with_message(&longer);
        assert_eq!(
            cut.message.as_deref().map(|m| m.chars().count()),
            Some(MAX_MESSAGE_CHARS)
        );
        assert_eq!(cut.message.as_deref(), Some(long.as_str()));
    }

    #[test]
    fn ok_and_error_are_exclusive() {
        for value in [
            json!({"type": "response", "id": "1", "ok": false}),
            json!({"type": "response", "id": "1", "ok": false,
                   "error": {"code": "Timeout"}, "result": {}}),
            json!({"type": "response", "id": "1", "ok": true, "error": {"code": "Timeout"}}),
        ] {
            assert!(parse(&value).is_err(), "{value}");
        }
        let ok = parse(&json!({"type": "response", "id": "1", "ok": true})).expect("no result");
        let Frame::Response(ok) = ok else {
            unreachable!()
        };
        assert_eq!(
            ok.outcome::<crate::EmptyResult>(),
            Ok(crate::EmptyResult {})
        );
        let failed =
            ResponseFrame::failure(RequestId::new("2").expect("id"), TransportError::Overloaded);
        assert_eq!(
            failed.outcome::<crate::EmptyResult>(),
            Err(TransportError::Overloaded)
        );
    }

    #[test]
    fn every_class_has_one_sender() {
        let id = || RequestId::new("1").expect("id");
        let nonce = Nonce::new("_-0123456789abcdefghij").expect("nonce");
        let ping = Ping::new(nonce);
        let client = [
            parse(
                &json!({"type": "hello", "ipc_version": {"major": 2, "minor": 0},
                          "client": {"kind": "k"}}),
            )
            .expect("hello"),
            Frame::Request(crate::Request::AdminStatus.into_frame(id(), None)),
            Frame::Cancel(Cancel::new(id())),
            Frame::Pong(ping.echo()),
        ];
        let server = [
            Frame::Close(Close::new(TransportError::ShuttingDown)),
            Frame::Response(ResponseFrame::failure(id(), TransportError::Timeout)),
            Frame::ServerState(ServerState::new(Health::Healthy, None)),
            Frame::Ping(ping),
        ];
        assert!(client.iter().all(|f| f.sender() == Sender::Client));
        assert!(server.iter().all(|f| f.sender() == Sender::Server));
    }

    #[test]
    fn a_nonce_outside_its_grammar_is_refused() {
        for nonce in [
            "short",
            &"a".repeat(65),
            "has space in it!!",
            "0123456789abcde=",
        ] {
            assert!(Nonce::new(nonce).is_err(), "{nonce}");
        }
        assert!(Nonce::new("a".repeat(16)).is_ok());
    }

    #[test]
    fn the_hello_timeout_is_five_seconds() {
        assert_eq!(HELLO_TIMEOUT, Duration::from_secs(5));
    }
}
