// The connection loop reads these; until it lands, the expectation fails
// the build the moment it is met, so it cannot outlive its reason.
// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! One admitted request, one port call (plan §16 (1), (5)).
//!
//! Nothing is decided here that the port does not decide: the capability
//! was judged at admission, and every refusal past it -- a missing lease,
//! an unjoined channel, a peer out of trust -- is the binding's answer,
//! carried back as the response's code. This is the whole of what makes
//! IPC a serialization of the port rather than a second behaviour model.

#![cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "read by the connection loop, a later commit of this batch"
    )
)]

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use interweave_ipc_protocol::{
    AdminStatusResult, DirectoryResult, EmptyResult, EndpointList, Request, RequestId,
    ResponseFrame, SendResult, SetEnabledResult,
};
use interweave_local_client_api::{AdminPort, DataSessionPort};
use interweave_transport_api::{BroadcastMessageV1, DirectDestination, TransportError};

use crate::counters::Counters;

/// Answer a data-socket request through its session.
pub(crate) async fn data<S: DataSessionPort>(
    session: &S,
    id: RequestId,
    request: Request,
) -> ResponseFrame {
    let empty = |id, outcome: Result<(), TransportError>| match outcome {
        Ok(()) => ResponseFrame::success(id, &EmptyResult {}),
        Err(code) => ResponseFrame::failure(id, code),
    };
    match request {
        Request::ChannelJoin(p) => empty(id, session.join(p.channel).await),
        Request::ChannelLeave(p) => empty(id, session.leave(p.channel).await),
        Request::BroadcastPublish(p) => {
            let message = BroadcastMessageV1 {
                message_id: p.message_id,
                // Diagnostic only (the field's own contract): no admission
                // path reads it.
                sent_at_ms: wall_ms(),
                payload: p.payload,
            };
            empty(id, session.broadcast(p.channel, message).await)
        }
        Request::DirectSend(p) => {
            // Absent asks the remote for its configured default, never
            // fan-out (ADR-0030).
            let destination = match p.endpoint {
                Some(endpoint) => DirectDestination::to_endpoint(p.peer, endpoint),
                None => DirectDestination::to_default(p.peer),
            };
            match session
                .send_direct(destination, p.message_id, p.payload)
                .await
            {
                Ok(resolved_endpoint) => {
                    ResponseFrame::success(id, &SendResult { resolved_endpoint })
                }
                Err(code) => ResponseFrame::failure(id, code),
            }
        }
        Request::EndpointsQuery(p) => match session.query_endpoints(p.peer).await {
            Ok(directory) => ResponseFrame::success(id, &DirectoryResult::from(directory)),
            Err(code) => ResponseFrame::failure(id, code),
        },
        // Admission refuses the admin domain on the data socket before
        // dispatch; reaching here would be admission's bug, answered as
        // the refusal it should have been.
        Request::AdminStatus
        | Request::AdminEndpointsList
        | Request::AdminEndpointsRevoke(_)
        | Request::AdminEndpointsSetEnabled(_)
        | Request::AdminEndpointsSetDefault(_)
        | Request::AdminShutdown(_) => ResponseFrame::failure(id, TransportError::CapabilityDenied),
    }
}

/// Answer an admin-socket request through its port.
pub(crate) async fn admin<A: AdminPort>(
    port: &A,
    counters: &Counters,
    default_grace: Duration,
    id: RequestId,
    request: Request,
) -> ResponseFrame {
    let empty = |id, outcome: Result<(), TransportError>| match outcome {
        Ok(()) => ResponseFrame::success(id, &EmptyResult {}),
        Err(code) => ResponseFrame::failure(id, code),
    };
    match request {
        Request::AdminStatus => match port.status().await {
            Ok(status) => {
                ResponseFrame::success(id, &AdminStatusResult::new(status, counters.snapshot()))
            }
            Err(code) => ResponseFrame::failure(id, code),
        },
        Request::AdminEndpointsList => {
            match port.leases().await.and_then(EndpointList::from_views) {
                Ok(list) => ResponseFrame::success(id, &list),
                Err(code) => ResponseFrame::failure(id, code),
            }
        }
        Request::AdminEndpointsRevoke(p) => empty(id, port.revoke_endpoint(p.endpoint).await),
        Request::AdminEndpointsSetEnabled(p) => {
            match port.set_endpoint_enabled(p.endpoint, p.enabled).await {
                Ok(revoked_epoch) => {
                    ResponseFrame::success(id, &SetEnabledResult { revoked_epoch })
                }
                Err(code) => ResponseFrame::failure(id, code),
            }
        }
        Request::AdminEndpointsSetDefault(p) => {
            empty(id, port.set_default_endpoint(p.endpoint).await)
        }
        Request::AdminShutdown(p) => {
            let grace = p
                .grace_ms
                .map_or(default_grace, |ms| Duration::from_millis(u64::from(ms)));
            empty(id, port.shutdown(grace).await)
        }
        // The data domain on the admin socket: admission's to refuse, as
        // above.
        Request::ChannelJoin(_)
        | Request::ChannelLeave(_)
        | Request::BroadcastPublish(_)
        | Request::DirectSend(_)
        | Request::EndpointsQuery(_) => {
            ResponseFrame::failure(id, TransportError::CapabilityDenied)
        }
    }
}

fn wall_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]
    use super::*;
    use crate::fake::{Fake, PEER};
    use interweave_ipc_protocol::{Frame, Method};
    use interweave_local_client_api::{
        AdminBinding as _, DataCapability, DataSessionBinding as _, SessionRequest,
    };
    use interweave_transport_api::EndpointId;

    fn request(method: Method, params: &serde_json::Value) -> Request {
        let raw = serde_json::value::to_raw_value(params).expect("raw");
        Request::decode(method, Some(&raw)).expect("a well-formed request")
    }

    fn id() -> RequestId {
        RequestId::new("r").expect("id")
    }

    fn body(response: ResponseFrame) -> String {
        Frame::Response(response).to_body()
    }

    async fn leased(
        fake: &Fake,
    ) -> <Fake as interweave_local_client_api::DataSessionBinding>::Session {
        fake.open(
            SessionRequest::new(
                "human-client",
                Some(EndpointId::parse("human").expect("endpoint")),
                [
                    DataCapability::Commands,
                    DataCapability::Events,
                    DataCapability::EndpointsQuery,
                ],
            )
            .expect("request"),
        )
        .await
        .expect("session")
    }

    /// Each data method is exactly one call of its port method, and the
    /// result is the schema's.
    #[tokio::test]
    async fn every_data_method_is_one_port_call() {
        let fake = Fake::default();
        let session = leased(&fake).await;
        let payload = serde_json::json!({"bytes": "aGk"});
        let mid = "00000000000000000000000000000001";
        let cases = [
            (
                Method::ChannelJoin,
                serde_json::json!({"channel": "ops"}),
                "join ops",
                r#""result":{}"#,
            ),
            (
                Method::ChannelLeave,
                serde_json::json!({"channel": "ops"}),
                "leave ops",
                r#""result":{}"#,
            ),
            (
                Method::BroadcastPublish,
                serde_json::json!({"channel": "ops", "message_id": mid, "payload": payload}),
                "broadcast ops",
                r#""result":{}"#,
            ),
            (
                Method::DirectSend,
                serde_json::json!({"peer": PEER, "message_id": mid, "payload": payload}),
                "send DirectDestination",
                r#""resolved_endpoint":"remote""#,
            ),
            (
                Method::EndpointsQuery,
                serde_json::json!({"peer": PEER}),
                "query ",
                r#""endpoints":["agent"]"#,
            ),
        ];
        for (method, params, call, result) in cases {
            let before = fake.script().calls.len();
            let answer = body(data(&session, id(), request(method, &params)).await);
            let calls = fake.script().calls[before..].to_vec();
            assert_eq!(calls.len(), 1, "{}: {calls:?}", method.as_str());
            assert!(calls[0].starts_with(call), "{}: {calls:?}", method.as_str());
            assert!(answer.contains(result), "{}: {answer}", method.as_str());
        }
    }

    /// Absent asks for the remote's default; present names it. The port
    /// sees which, and never a fan-out.
    #[tokio::test]
    async fn an_absent_destination_endpoint_asks_for_the_default() {
        let fake = Fake::default();
        let session = leased(&fake).await;
        let base = serde_json::json!({"peer": PEER, "message_id": "00000000000000000000000000000001",
                                      "payload": {"bytes": ""}});
        let _ = data(&session, id(), request(Method::DirectSend, &base)).await;
        let mut named = base.clone();
        named["endpoint"] = serde_json::json!("agent");
        let _ = data(&session, id(), request(Method::DirectSend, &named)).await;
        let calls = fake.script().calls.clone();
        let sends: Vec<_> = calls.iter().filter(|c| c.starts_with("send")).collect();
        assert_eq!(sends.len(), 2);
        assert!(sends[0].contains("endpoint: None"), "{}", sends[0]);
        assert!(sends[1].contains("agent"), "{}", sends[1]);
    }

    /// The port's refusal is the answer: here a send from a session with
    /// no lease is the binding's `EndpointNotRegistered`.
    #[tokio::test]
    async fn the_ports_refusal_is_the_responses_code() {
        let fake = Fake::default();
        let session = fake
            .open(SessionRequest::new("diag", None, [DataCapability::Commands]).expect("request"))
            .await
            .expect("session");
        let params = serde_json::json!({"peer": PEER, "message_id": "00000000000000000000000000000001",
                                        "payload": {"bytes": ""}});
        let answer = body(data(&session, id(), request(Method::DirectSend, &params)).await);
        assert!(answer.contains("EndpointNotRegistered"), "{answer}");
    }

    #[tokio::test]
    async fn every_admin_method_is_one_port_call_and_status_carries_the_servers_counters() {
        let fake = Fake::default();
        let port = fake.admin([].into()).await.expect("port");
        let counters = Counters::default();
        let grace = Duration::from_millis(1234);
        let status = body(admin(&port, &counters, grace, id(), Request::AdminStatus).await);
        assert!(status.contains(r#""data_connections":0"#), "{status}");
        let cases = [
            (Method::AdminEndpointsList, serde_json::json!({}), "leases"),
            (
                Method::AdminEndpointsRevoke,
                serde_json::json!({"endpoint": "human"}),
                "revoke human",
            ),
            (
                Method::AdminEndpointsSetEnabled,
                serde_json::json!({"endpoint": "human", "enabled": false}),
                "set_enabled human false",
            ),
            (
                Method::AdminEndpointsSetDefault,
                serde_json::json!({"endpoint": null}),
                "set_default None",
            ),
            (
                Method::AdminShutdown,
                serde_json::json!({}),
                "shutdown 1234",
            ),
            (
                Method::AdminShutdown,
                serde_json::json!({"grace_ms": 7}),
                "shutdown 7",
            ),
        ];
        for (method, params, call) in cases {
            let before = fake.script().calls.len();
            let answer = body(admin(&port, &counters, grace, id(), request(method, &params)).await);
            assert!(
                answer.contains(r#""ok":true"#),
                "{}: {answer}",
                method.as_str()
            );
            assert_eq!(
                fake.script().calls[before..],
                [call.to_owned()],
                "{}",
                method.as_str()
            );
        }
    }

    /// A request of the other domain never reaches its port, even if
    /// admission were bypassed.
    #[tokio::test]
    async fn the_other_domains_request_reaches_no_port() {
        let fake = Fake::default();
        let session = leased(&fake).await;
        let port = fake.admin([].into()).await.expect("port");
        let before = fake.script().calls.len();
        let answer = body(data(&session, id(), Request::AdminStatus).await);
        assert!(answer.contains("CapabilityDenied"), "{answer}");
        let join = request(Method::ChannelJoin, &serde_json::json!({"channel": "ops"}));
        let answer = body(admin(&port, &Counters::default(), Duration::ZERO, id(), join).await);
        assert!(answer.contains("CapabilityDenied"), "{answer}");
        assert_eq!(fake.script().calls.len(), before);
    }
}
