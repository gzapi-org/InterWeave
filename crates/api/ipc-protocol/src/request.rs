// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! Request frames, their params (`ipc/request.schema.json` and the params
//! schemas it references), and admission: the ordered checks a request
//! passes before anything is dispatched (`LOCAL-IPC.md` §Method catalogue,
//! §Message classes).
//!
//! The envelope ([`RequestFrame`]) carries the method as TEXT and the
//! params as an uninterpreted object, because two of the refusals are
//! answers on a connection that stays open: a name outside the catalogue
//! (`ProtocolUnsupported`) and params that do not match their method
//! (`InvalidArgument`). A typed envelope would turn both into a frame that
//! failed to parse, which has no request id to answer.

use std::collections::BTreeSet;

use interweave_local_client_api::{AdminCapability, DataCapability};
use interweave_transport_api::{
    ChannelId, EndpointId, MAX_PAYLOAD_BYTES, MessageId, Payload, TransportError, TransportIdentity,
};
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;

use crate::catalogue::Method;
use crate::handshake::AuthorityDomain;
use crate::version::IpcVersion;

/// Maximum CHARACTERS in a request id (`ipc/frame.schema.json`
/// `request_id`, `maxLength`, which counts code points; no prose bounds
/// it in bytes).
pub const MAX_REQUEST_ID_CHARS: usize = 128;

/// The longest `grace_ms` an `admin.shutdown` may ask for.
pub const MAX_SHUTDOWN_GRACE_MS: u32 = 600_000;

/// A request id: 1..=128 characters, unique per connection, never durable.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct RequestId(String);

impl RequestId {
    /// Wrap a request id.
    ///
    /// # Errors
    /// [`TransportError::InvalidArgument`] outside 1..=128 characters.
    pub fn new(id: impl Into<String>) -> Result<Self, TransportError> {
        let id = id.into();
        if id.is_empty() || id.chars().count() > MAX_REQUEST_ID_CHARS {
            return Err(TransportError::InvalidArgument);
        }
        Ok(Self(id))
    }

    /// The id's text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for RequestId {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Self::new(String::deserialize(d)?)
            .map_err(|_| serde::de::Error::custom("a request id is 1..=128 characters"))
    }
}

/// The literal `"request"` discriminant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RequestTag {
    /// The only legal value.
    #[serde(rename = "request")]
    Request,
}

/// A `request` frame as it arrives, before admission.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequestFrame {
    /// Always `"request"`.
    #[serde(rename = "type")]
    pub frame_type: RequestTag,
    /// The id every answer to this request carries.
    pub id: RequestId,
    /// The method's wire name, judged by [`RequestFrame::admit`].
    pub method: String,
    /// The params object as it arrived, judged against the method's
    /// shape at admission.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::raw::absent_or_object"
    )]
    pub params: Option<Box<RawValue>>,
    /// An optional deadline for the operation, in milliseconds.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "absent_or_u64"
    )]
    pub deadline_ms: Option<u64>,
}

/// `channel.join` and `channel.leave` params.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChannelParams {
    /// The channel.
    pub channel: ChannelId,
}

/// `broadcast.publish` params.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublishParams {
    /// A joined channel.
    pub channel: ChannelId,
    /// The application's message id.
    pub message_id: MessageId,
    /// The payload.
    pub payload: Payload,
}

/// `direct.send` params. The SOURCE endpoint is not here and cannot be:
/// it is the connection's lease (ADR-0030).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SendParams {
    /// The destination peer.
    pub peer: TransportIdentity,
    /// The REMOTE destination endpoint; absent asks for the remote's
    /// configured default, never fan-out.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "absent_or_endpoint"
    )]
    pub endpoint: Option<EndpointId>,
    /// The application's message id.
    pub message_id: MessageId,
    /// The payload.
    pub payload: Payload,
}

/// `endpoints.query` params.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QueryParams {
    /// The trusted peer whose directory is asked.
    pub peer: TransportIdentity,
}

/// `admin.endpoints.revoke` params.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EndpointParams {
    /// The configured endpoint.
    pub endpoint: EndpointId,
}

/// `admin.endpoints.set_enabled` params.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetEnabledParams {
    /// The configured endpoint.
    pub endpoint: EndpointId,
    /// Disabling revokes a live lease at once and never auto-rebinds.
    pub enabled: bool,
}

/// `admin.endpoints.set_default` params.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetDefaultParams {
    /// The new default, or `null` to clear it. The key is REQUIRED:
    /// absent is not "clear", it is a malformed request.
    #[serde(deserialize_with = "required_nullable_endpoint")]
    pub endpoint: Option<EndpointId>,
}

/// `admin.shutdown` params.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShutdownParams {
    /// How long bounded direct responses may settle, at most
    /// [`MAX_SHUTDOWN_GRACE_MS`]; absent means the daemon's default.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "absent_or_grace"
    )]
    pub grace_ms: Option<u32>,
}

/// The params a method taking none accepts: `{}` or nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct EmptyParams {}

/// A request, its method and params bound together.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    /// `channel.join`.
    ChannelJoin(ChannelParams),
    /// `channel.leave`.
    ChannelLeave(ChannelParams),
    /// `broadcast.publish`.
    BroadcastPublish(PublishParams),
    /// `direct.send`.
    DirectSend(SendParams),
    /// `endpoints.query`.
    EndpointsQuery(QueryParams),
    /// `admin.status`.
    AdminStatus,
    /// `admin.endpoints.list`.
    AdminEndpointsList,
    /// `admin.endpoints.revoke`.
    AdminEndpointsRevoke(EndpointParams),
    /// `admin.endpoints.set_enabled`.
    AdminEndpointsSetEnabled(SetEnabledParams),
    /// `admin.endpoints.set_default`.
    AdminEndpointsSetDefault(SetDefaultParams),
    /// `admin.shutdown`.
    AdminShutdown(ShutdownParams),
}

impl Request {
    /// The request's method.
    #[must_use]
    pub const fn method(&self) -> Method {
        match self {
            Self::ChannelJoin(_) => Method::ChannelJoin,
            Self::ChannelLeave(_) => Method::ChannelLeave,
            Self::BroadcastPublish(_) => Method::BroadcastPublish,
            Self::DirectSend(_) => Method::DirectSend,
            Self::EndpointsQuery(_) => Method::EndpointsQuery,
            Self::AdminStatus => Method::AdminStatus,
            Self::AdminEndpointsList => Method::AdminEndpointsList,
            Self::AdminEndpointsRevoke(_) => Method::AdminEndpointsRevoke,
            Self::AdminEndpointsSetEnabled(_) => Method::AdminEndpointsSetEnabled,
            Self::AdminEndpointsSetDefault(_) => Method::AdminEndpointsSetDefault,
            Self::AdminShutdown(_) => Method::AdminShutdown,
        }
    }

    /// Bind `params` to `method`'s shape.
    ///
    /// # Errors
    /// [`TransportError::PayloadTooLarge`] for a payload over the
    /// architecture ceiling, [`TransportError::InvalidArgument`] for
    /// anything else that is not the method's shape -- including a
    /// missing `params` where the method requires one.
    pub fn decode(method: Method, params: Option<&RawValue>) -> Result<Self, TransportError> {
        fn typed<T: serde::de::DeserializeOwned>(
            params: Option<&RawValue>,
        ) -> Result<T, TransportError> {
            let params = params.ok_or(TransportError::InvalidArgument)?;
            crate::strict::from_str(params.get()).map_err(|_| TransportError::InvalidArgument)
        }
        // A method taking none accepts an absent `params` or `{}`
        // (`ipc/request.schema.json`: `params` is not required there).
        fn none(params: Option<&RawValue>) -> Result<(), TransportError> {
            params.map_or(Ok(()), |p| {
                typed::<EmptyParams>(Some(p)).map(|EmptyParams {}| ())
            })
        }
        // Only a method that carries a payload can be answered
        // PayloadTooLarge: a stray `payload` key on any other is simply not
        // its shape.
        if matches!(method, Method::BroadcastPublish | Method::DirectSend)
            && payload_over_ceiling(params)
        {
            return Err(TransportError::PayloadTooLarge);
        }
        Ok(match method {
            Method::ChannelJoin => Self::ChannelJoin(typed(params)?),
            Method::ChannelLeave => Self::ChannelLeave(typed(params)?),
            Method::BroadcastPublish => Self::BroadcastPublish(typed(params)?),
            Method::DirectSend => Self::DirectSend(typed(params)?),
            Method::EndpointsQuery => Self::EndpointsQuery(typed(params)?),
            Method::AdminStatus => none(params).map(|()| Self::AdminStatus)?,
            Method::AdminEndpointsList => none(params).map(|()| Self::AdminEndpointsList)?,
            Method::AdminEndpointsRevoke => Self::AdminEndpointsRevoke(typed(params)?),
            Method::AdminEndpointsSetEnabled => Self::AdminEndpointsSetEnabled(typed(params)?),
            Method::AdminEndpointsSetDefault => Self::AdminEndpointsSetDefault(typed(params)?),
            Method::AdminShutdown => Self::AdminShutdown(typed(params)?),
        })
    }

    /// The params object this request sends: a method taking none sends
    /// `{}`, as the golden frames do. Its keys are in the params type's
    /// field order, the schema's.
    ///
    /// # Panics
    /// Never: every params type serializes.
    #[must_use]
    pub fn params(&self) -> Box<RawValue> {
        let raw = match self {
            Self::ChannelJoin(p) | Self::ChannelLeave(p) => serde_json::value::to_raw_value(p),
            Self::BroadcastPublish(p) => serde_json::value::to_raw_value(p),
            Self::DirectSend(p) => serde_json::value::to_raw_value(p),
            Self::EndpointsQuery(p) => serde_json::value::to_raw_value(p),
            Self::AdminStatus | Self::AdminEndpointsList => {
                serde_json::value::to_raw_value(&EmptyParams {})
            }
            Self::AdminEndpointsRevoke(p) => serde_json::value::to_raw_value(p),
            Self::AdminEndpointsSetEnabled(p) => serde_json::value::to_raw_value(p),
            Self::AdminEndpointsSetDefault(p) => serde_json::value::to_raw_value(p),
            Self::AdminShutdown(p) => serde_json::value::to_raw_value(p),
        };
        raw.unwrap_or_else(|_| unreachable!("a params type serializes"))
    }

    /// The `request` frame carrying this request.
    #[must_use]
    pub fn into_frame(self, id: RequestId, deadline_ms: Option<u64>) -> RequestFrame {
        RequestFrame {
            frame_type: RequestTag::Request,
            id,
            method: self.method().as_str().to_owned(),
            params: Some(self.params()),
            deadline_ms,
        }
    }
}

/// Why a request was not admitted. Every refusal is a `response{ok:
/// false}` on a connection that stays: none of these is a `close`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// A name outside the catalogue, or one introduced by a minor above
    /// the negotiated one.
    Unsupported,
    /// A method of the other authority domain (ADR-0037): refused before
    /// dispatch and counted as `cross_domain_capability_denied_total`.
    CrossDomain,
    /// The connection does not hold the method's capability.
    NotGranted,
    /// The params do not fit the method.
    Params(TransportError),
}

impl Refusal {
    /// The error code the response carries.
    #[must_use]
    pub const fn code(self) -> TransportError {
        match self {
            Self::Unsupported => TransportError::ProtocolUnsupported,
            Self::CrossDomain | Self::NotGranted => TransportError::CapabilityDenied,
            Self::Params(code) => code,
        }
    }
}

/// What a connection holds once its hello was answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Admission<'a> {
    /// The socket it arrived on.
    pub domain: AuthorityDomain,
    /// The negotiated version.
    pub version: IpcVersion,
    /// Its data grants.
    pub granted_data: &'a BTreeSet<DataCapability>,
    /// Its admin grants.
    pub granted_admin: &'a BTreeSet<AdminCapability>,
}

impl RequestFrame {
    /// Admit this request, in the contract's order: a name the connection
    /// does not know, then the other domain's method, then an ungranted
    /// capability, then the params. Authority is judged before the
    /// params, so a connection without a grant learns nothing about what
    /// a well-formed request for it would be.
    ///
    /// # Errors
    /// The [`Refusal`] the response carries.
    pub fn admit(&self, conn: &Admission<'_>) -> Result<Request, Refusal> {
        let method = Method::parse(&self.method)
            .filter(|m| m.available_at(conn.version))
            .ok_or(Refusal::Unsupported)?;
        let entry = method.entry();
        if entry.domain != conn.domain {
            return Err(Refusal::CrossDomain);
        }
        let granted = match (entry.capability.as_data(), entry.capability.as_admin()) {
            (Some(data), _) => conn.granted_data.contains(&data),
            (None, Some(admin)) => conn.granted_admin.contains(&admin),
            (None, None) => false,
        };
        if !granted {
            return Err(Refusal::NotGranted);
        }
        Request::decode(method, self.params.as_deref()).map_err(Refusal::Params)
    }
}

/// Whether `params.payload.bytes` is longer than any payload under the
/// ceiling encodes to. Judged on the raw text so that the refusal names
/// `PayloadTooLarge` without reading an error message, and before any
/// decode allocates.
fn payload_over_ceiling(params: Option<&RawValue>) -> bool {
    #[derive(Deserialize)]
    struct Peek<'a> {
        #[serde(borrow)]
        payload: Option<PeekPayload<'a>>,
    }
    #[derive(Deserialize)]
    struct PeekPayload<'a> {
        #[serde(borrow)]
        bytes: Option<std::borrow::Cow<'a, str>>,
    }
    let max_encoded = MAX_PAYLOAD_BYTES.div_ceil(3) * 4;
    params
        .and_then(|p| crate::strict::from_str::<Peek<'_>>(p.get()).ok())
        .and_then(|peek| peek.payload?.bytes)
        .is_some_and(|bytes| bytes.len() > max_encoded)
}

fn absent_or_u64<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<u64>, D::Error> {
    u64::deserialize(d).map(Some)
}

fn absent_or_endpoint<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<Option<EndpointId>, D::Error> {
    EndpointId::deserialize(d).map(Some)
}

fn required_nullable_endpoint<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<Option<EndpointId>, D::Error> {
    Option::<EndpointId>::deserialize(d)
}

fn absent_or_grace<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<u32>, D::Error> {
    let grace = u32::deserialize(d)?;
    if grace > MAX_SHUTDOWN_GRACE_MS {
        return Err(serde::de::Error::custom(format!(
            "grace_ms is at most {MAX_SHUTDOWN_GRACE_MS}"
        )));
    }
    Ok(Some(grace))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn raw(value: &serde_json::Value) -> Box<RawValue> {
        serde_json::value::to_raw_value(value).expect("raw")
    }

    const PEER: &str = "12D3KooWDpJ7As7BWAwRMfu1VU2WCqNjvq387JEYKDBj4kx6nXTN";

    fn frame(value: &serde_json::Value) -> RequestFrame {
        serde_json::from_str(&value.to_string()).expect("a request envelope")
    }

    fn data(granted: &BTreeSet<DataCapability>) -> Admission<'_> {
        static NONE: BTreeSet<AdminCapability> = BTreeSet::new();
        Admission {
            domain: AuthorityDomain::Data,
            version: IpcVersion { major: 2, minor: 0 },
            granted_data: granted,
            granted_admin: &NONE,
        }
    }

    fn admin(granted: &BTreeSet<AdminCapability>) -> Admission<'_> {
        static NONE: BTreeSet<DataCapability> = BTreeSet::new();
        Admission {
            domain: AuthorityDomain::Admin,
            version: IpcVersion { major: 2, minor: 0 },
            granted_data: &NONE,
            granted_admin: granted,
        }
    }

    fn commands() -> BTreeSet<DataCapability> {
        [DataCapability::Commands].into()
    }

    #[test]
    fn a_granted_request_is_admitted_with_its_params_bound() {
        let caps = commands();
        let request = frame(&json!({
            "type": "request", "id": "1", "method": "channel.join",
            "params": {"channel": "ops"}
        }))
        .admit(&data(&caps))
        .expect("admitted");
        assert_eq!(
            request,
            Request::ChannelJoin(ChannelParams {
                channel: ChannelId::parse("ops").expect("channel")
            })
        );
    }

    #[test]
    fn the_refusals_come_in_the_contracts_order() {
        let caps = commands();
        let conn = data(&caps);
        let refused = |value: serde_json::Value| frame(&value).admit(&conn).expect_err("refused");
        // Unknown name: answered, the connection stays.
        assert_eq!(
            refused(json!({"type": "request", "id": "1", "method": "admin.trust.add"})),
            Refusal::Unsupported
        );
        // The admin domain on the data socket, whatever the params: the
        // CROSS-DOMAIN refusal, which the server counts, not merely one
        // with the same code (`NotGranted` answers CapabilityDenied too).
        let cross = refused(
            json!({"type": "request", "id": "2", "method": "admin.shutdown",
                                   "params": {"grace_ms": "soon"}}),
        );
        assert_eq!(cross, Refusal::CrossDomain);
        assert_eq!(cross.code(), TransportError::CapabilityDenied);
        // Ungranted before the params are read: a malformed
        // `endpoints.query` from a connection without the grant is
        // NotGranted, not InvalidArgument.
        assert_eq!(
            refused(
                json!({"type": "request", "id": "3", "method": "endpoints.query",
                           "params": {"peer": "not a peer"}})
            ),
            Refusal::NotGranted
        );
        assert_eq!(
            refused(
                json!({"type": "request", "id": "4", "method": "channel.join",
                           "params": {"channel": "ops", "extra": 1}})
            ),
            Refusal::Params(TransportError::InvalidArgument)
        );
    }

    #[test]
    fn the_admin_socket_refuses_every_data_method_as_cross_domain() {
        let caps: BTreeSet<AdminCapability> = [
            AdminCapability::Status,
            AdminCapability::Endpoints,
            AdminCapability::Shutdown,
        ]
        .into();
        let conn = admin(&caps);
        for method in Method::ALL {
            let admitted = frame(&json!({"type": "request", "id": "1", "method": method.as_str()}))
                .admit(&conn);
            if method.entry().domain == AuthorityDomain::Data {
                assert_eq!(admitted, Err(Refusal::CrossDomain), "{}", method.as_str());
            } else {
                assert_ne!(admitted, Err(Refusal::CrossDomain), "{}", method.as_str());
            }
        }
    }

    #[test]
    fn a_method_above_the_negotiated_minor_is_unsupported() {
        // Every 2.0 method is available at 2.0; a connection at a minor
        // below a method's is refused as if the name were unknown.
        let caps = commands();
        let conn = data(&caps);
        assert!(
            frame(
                &json!({"type": "request", "id": "1", "method": "channel.leave",
                         "params": {"channel": "ops"}})
            )
            .admit(&conn)
            .is_ok()
        );
    }

    #[test]
    fn a_method_taking_no_params_accepts_absence_and_the_empty_object_only() {
        for params in [None, Some(raw(&json!({})))] {
            assert_eq!(
                Request::decode(Method::AdminStatus, params.as_deref()),
                Ok(Request::AdminStatus)
            );
        }
        assert_eq!(
            Request::decode(
                Method::AdminEndpointsList,
                Some(&raw(&json!({"verbose": true})))
            ),
            Err(TransportError::InvalidArgument)
        );
        // And a method that takes params needs them present, even when
        // every field is optional (`admin.shutdown`).
        assert_eq!(
            Request::decode(Method::AdminShutdown, None),
            Err(TransportError::InvalidArgument)
        );
    }

    #[test]
    fn set_default_needs_its_key_and_null_clears() {
        let decode = |value: serde_json::Value| {
            Request::decode(Method::AdminEndpointsSetDefault, Some(&raw(&value)))
        };
        assert_eq!(
            decode(json!({"endpoint": null})),
            Ok(Request::AdminEndpointsSetDefault(SetDefaultParams {
                endpoint: None
            }))
        );
        assert_eq!(decode(json!({})), Err(TransportError::InvalidArgument));
    }

    #[test]
    fn an_absent_optional_is_not_a_null_one() {
        let decode = |method, value: serde_json::Value| Request::decode(method, Some(&raw(&value)));
        assert_eq!(
            decode(Method::AdminShutdown, json!({"grace_ms": null})),
            Err(TransportError::InvalidArgument)
        );
        assert_eq!(
            decode(
                Method::DirectSend,
                json!({"peer": PEER, "endpoint": null,
                       "message_id": "00000000000000000000000000000001",
                       "payload": {"bytes": ""}})
            ),
            Err(TransportError::InvalidArgument)
        );
    }

    #[test]
    fn a_frame_with_a_null_params_or_deadline_is_malformed() {
        for value in [
            json!({"type": "request", "id": "1", "method": "admin.status", "params": null}),
            json!({"type": "request", "id": "1", "method": "admin.status", "deadline_ms": null}),
            json!({"type": "request", "id": "1", "method": "admin.status", "params": []}),
            json!({"type": "request", "id": "1", "method": "admin.status", "params": "{}"}),
            json!({"type": "request", "id": "", "method": "admin.status"}),
            json!({"type": "request", "id": "x".repeat(129), "method": "admin.status"}),
        ] {
            assert!(
                serde_json::from_str::<RequestFrame>(&value.to_string()).is_err(),
                "{value}"
            );
        }
    }

    #[test]
    fn shutdown_grace_is_bounded() {
        let decode = |grace: u64| {
            Request::decode(
                Method::AdminShutdown,
                Some(&raw(&json!({"grace_ms": grace}))),
            )
        };
        assert_eq!(
            decode(u64::from(MAX_SHUTDOWN_GRACE_MS)),
            Ok(Request::AdminShutdown(ShutdownParams {
                grace_ms: Some(MAX_SHUTDOWN_GRACE_MS)
            }))
        );
        assert_eq!(
            decode(u64::from(MAX_SHUTDOWN_GRACE_MS) + 1),
            Err(TransportError::InvalidArgument)
        );
    }

    #[test]
    fn a_payload_past_the_ceiling_is_payload_too_large() {
        let send = |encoded: usize| {
            let params = json!({
                "peer": PEER,
                "message_id": "00000000000000000000000000000001",
                "payload": {"bytes": "A".repeat(encoded)}
            });
            Request::decode(Method::DirectSend, Some(&raw(&params)))
        };
        // 65,536 characters decode to exactly 49,152 bytes: the ceiling.
        assert!(send(65_536).is_ok());
        assert_eq!(send(65_540), Err(TransportError::PayloadTooLarge));
        // The other payload method answers it too.
        let publish = json!({"channel": "ops", "message_id": "00000000000000000000000000000001",
                             "payload": {"bytes": "A".repeat(65_540)}});
        assert_eq!(
            Request::decode(Method::BroadcastPublish, Some(&raw(&publish))),
            Err(TransportError::PayloadTooLarge)
        );
        // An oversized payload in the wrong SHAPE is the wrong shape: the
        // peek is as strict as the decode, so an array is not measured.
        let array = json!({"peer": PEER, "message_id": "00000000000000000000000000000001",
                           "payload": ["A".repeat(65_540)]});
        assert_eq!(
            Request::decode(Method::DirectSend, Some(&raw(&array))),
            Err(TransportError::InvalidArgument)
        );
        // A method with no payload in its shape answers the stray key as
        // what it is: not its params.
        let join = json!({"channel": "ops", "payload": {"bytes": "A".repeat(65_540)}});
        assert_eq!(
            Request::decode(Method::ChannelJoin, Some(&raw(&join))),
            Err(TransportError::InvalidArgument)
        );
    }

    #[test]
    fn every_request_round_trips_through_its_frame() {
        let payload = || Payload::at_ceiling(None, b"hi".to_vec()).expect("payload");
        let peer = || TransportIdentity::parse(PEER).expect("peer");
        let ep = || EndpointId::parse("human").expect("endpoint");
        let channel = || ChannelId::parse("ops").expect("channel");
        let id = MessageId::from_bytes([7; 16]);
        let requests = [
            Request::ChannelJoin(ChannelParams { channel: channel() }),
            Request::ChannelLeave(ChannelParams { channel: channel() }),
            Request::BroadcastPublish(PublishParams {
                channel: channel(),
                message_id: id,
                payload: payload(),
            }),
            Request::DirectSend(SendParams {
                peer: peer(),
                endpoint: Some(ep()),
                message_id: id,
                payload: payload(),
            }),
            Request::EndpointsQuery(QueryParams { peer: peer() }),
            Request::AdminStatus,
            Request::AdminEndpointsList,
            Request::AdminEndpointsRevoke(EndpointParams { endpoint: ep() }),
            Request::AdminEndpointsSetEnabled(SetEnabledParams {
                endpoint: ep(),
                enabled: false,
            }),
            Request::AdminEndpointsSetDefault(SetDefaultParams { endpoint: None }),
            Request::AdminShutdown(ShutdownParams { grace_ms: None }),
        ];
        assert_eq!(
            requests.iter().map(Request::method).collect::<Vec<_>>(),
            Method::ALL,
            "one request per method"
        );
        for request in requests {
            let wire = serde_json::to_string(
                &request
                    .clone()
                    .into_frame(RequestId::new("r").expect("id"), None),
            )
            .expect("ser");
            let back: RequestFrame = serde_json::from_str(&wire).expect("de");
            let method = Method::parse(&back.method).expect("known");
            assert_eq!(Request::decode(method, back.params.as_deref()), Ok(request));
        }
    }

    /// The raw field is judged by its first byte, which is the value's
    /// own: the parser strips the whitespace before it.
    #[test]
    fn whitespace_around_params_is_not_part_of_the_value() {
        let spaced = r#"{"type":"request","id":"1","method":"channel.join","params":   {"channel":"ops"}  }"#;
        let frame: RequestFrame = serde_json::from_str(spaced).expect("an object after spaces");
        assert_eq!(
            frame.params.as_deref().map(RawValue::get),
            Some(r#"{"channel":"ops"}"#)
        );
    }
}
