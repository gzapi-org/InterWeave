// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The hello phase (`LOCAL-IPC.md` §Handshake, §Version negotiation and
//! phases; plan §16 (4)).
//!
//! In the contract's order: a `hello` within [`HELLO_TIMEOUT`] as the
//! first frame, or `close{Timeout}` / `close{ProtocolViolation}`; the
//! major negotiated, or `close{VersionIncompatible, supported}`; a
//! capability above the negotiated minor closed `ProtocolViolation`; the
//! frame judged against the socket it arrived on (`Hello::evaluate`),
//! keepalive required for a lease checked THERE, before any lease exists;
//! then the session opened -- the lease claimed by the binding, which is
//! the only lease table -- or the admin port minted. A refusal at any
//! step is a `close` with its code and nothing held. Only then
//! `hello_response`, naming what was actually granted.

use std::collections::BTreeSet;
use std::num::NonZeroU32;
use std::time::Duration;

use interweave_ipc_protocol::{
    AuthorityDomain, Close, FEATURE_KEEPALIVE, Frame, GrantedLease, HELLO_TIMEOUT,
    HandshakeOutcome, HelloResponse, IpcVersion, negotiate,
};
use interweave_local_client_api::{
    AdminBinding, DataSessionBinding, DataSessionPort as _, SessionRequest,
};
use interweave_transport_api::{TransportError, TransportIdentity};
use tokio::io::AsyncRead;

use crate::admission::Limits;
use crate::frames::{FrameReader, ReadError};

/// `ipc.keepalive`, as the server runs it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeepalivePolicy {
    /// Whether the server offers keepalive at all.
    pub enabled: bool,
    /// Whether a lease claim must have negotiated it (profile default
    /// true): refused at claim time, never granted and revoked.
    pub required_for_lease: bool,
    /// Between probes (default 30 s).
    pub interval: Duration,
    /// How long an echo may take (default 10 s).
    pub response_timeout: Duration,
    /// Misses tolerated before the connection is closed (default 3).
    pub max_missed: u32,
}

impl Default for KeepalivePolicy {
    fn default() -> Self {
        Self {
            enabled: true,
            required_for_lease: true,
            interval: Duration::from_secs(30),
            response_timeout: Duration::from_secs(10),
            max_missed: 3,
        }
    }
}

/// What the server needs beyond the binding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerConfig {
    /// The profile's identity, as `hello_response` names it.
    pub peer: TransportIdentity,
    /// The client ceilings.
    pub limits: Limits,
    /// The keepalive.
    pub keepalive: KeepalivePolicy,
    /// The grace an `admin.shutdown` naming none is given: the daemon's
    /// default.
    pub shutdown_grace: Duration,
    /// A request's deadline when it names none: the profile's
    /// command-deadline default (TRANSPORT.md: 10 s).
    pub command_deadline: Duration,
    /// How long writing one frame to the client may take before the
    /// connection is closed as not reading ([`WRITE_STALL`] by default).
    pub write_stall: Duration,
}

/// The default [`ServerConfig::write_stall`]: a client that has not taken
/// a whole frame in this long is closed, and its slot and lease freed,
/// whether or not it negotiated keepalive. It bounds a frame, not the
/// time without progress, so a reader slower than about 13 KiB/s is
/// closed on a 128 KiB frame -- a rate no local client reads at.
pub const WRITE_STALL: Duration = Duration::from_secs(10);

/// The session's event queue bound as the wire's positive integer.
/// `LocalDataSession::new` refuses zero and caps the bound at
/// `MAX_EVENT_QUEUE`, so the fallback is never taken; it exists so a
/// bound past `u32` could not panic the connection.
fn granted_queue(bound: usize) -> NonZeroU32 {
    u32::try_from(bound)
        .ok()
        .and_then(NonZeroU32::new)
        .unwrap_or(NonZeroU32::MAX)
}

/// A connection past its hello.
pub(crate) enum Established<S, A> {
    /// A data-plane session, holding its lease if it claimed one.
    Data {
        session: S,
        version: IpcVersion,
        keepalive: bool,
    },
    /// An administrative port, never a lease.
    Admin {
        port: A,
        version: IpcVersion,
        keepalive: bool,
    },
}

/// What the hello phase ends with: the connection, if it opened, and the
/// one frame to write -- `hello_response` or a `close` -- if any.
pub(crate) type Outcome<S, A> = (Option<Established<S, A>>, Option<Frame>);

/// Run the hello phase. No connection when it ended in it -- closed with
/// a code, or gone -- and then nothing is held.
pub(crate) async fn hello<R, B>(
    reader: &mut FrameReader<R>,
    domain: AuthorityDomain,
    binding: &B,
    config: &ServerConfig,
) -> Outcome<B::Session, B::Admin>
where
    R: AsyncRead + Unpin,
    B: DataSessionBinding + AdminBinding,
{
    hello_within(reader, domain, binding, config, HELLO_TIMEOUT).await
}

pub(crate) async fn hello_within<R, B>(
    reader: &mut FrameReader<R>,
    domain: AuthorityDomain,
    binding: &B,
    config: &ServerConfig,
    window: Duration,
) -> Outcome<B::Session, B::Admin>
where
    R: AsyncRead + Unpin,
    B: DataSessionBinding + AdminBinding,
{
    let close = |code| (None, Some(Frame::Close(Close::new(code))));
    let body = match tokio::time::timeout(window, reader.next()).await {
        Err(_) => return close(TransportError::Timeout),
        Ok(Ok(Some(body))) => body,
        Ok(Err(ReadError::Frame(error))) => {
            let close =
                Close::new(TransportError::ProtocolViolation).with_message(&format!("{error:?}"));
            return (None, Some(Frame::Close(close)));
        }
        Ok(Ok(None) | Err(_)) => return (None, None),
    };
    // `hello` is the client's first frame and only its first: anything
    // else here is out of phase.
    let Ok(Frame::Hello(hello)) = Frame::parse(&body) else {
        return close(TransportError::ProtocolViolation);
    };
    let version = match negotiate(hello.ipc_version) {
        Ok(version) => version,
        Err(unsupported) => {
            return (
                None,
                Some(Frame::Close(Close::version_incompatible(unsupported))),
            );
        }
    };
    // A CAPABILITY ABOVE THE NEGOTIATED MINOR is the client's protocol
    // violation, on either socket and before the socket is asked about
    // it (LOCAL-IPC.md §Version negotiation): a 2.0 server's closed
    // capability parse refuses the same hello, so the client sees one
    // result whichever daemon it reached. Judged after the minor is
    // known, since only the minor makes such a hello wrong.
    if !hello.capabilities_available_at(version) {
        return close(TransportError::ProtocolViolation);
    }
    let required = config.keepalive.enabled && config.keepalive.required_for_lease;
    let outcome = match hello.evaluate(domain, required) {
        Ok(outcome) => outcome,
        Err(code) => return close(code),
    };
    let keepalive =
        config.keepalive.enabled && hello.features.iter().any(|f| f == FEATURE_KEEPALIVE);
    match domain {
        AuthorityDomain::Data => {
            let Ok(request) = SessionRequest::new(
                hello.client.kind.clone(),
                outcome.endpoint.clone(),
                outcome.granted_data.iter().copied(),
            ) else {
                return close(TransportError::InvalidArgument);
            };
            let session = match DataSessionBinding::open(binding, request).await {
                Ok(session) => session,
                Err(code) => return close(code),
            };
            let granted = HandshakeOutcome {
                granted_data: session.session().capabilities().clone(),
                granted_admin: BTreeSet::new(),
                endpoint: outcome.endpoint,
            };
            let lease = session
                .session()
                .endpoint_lease()
                .map(|lease| GrantedLease {
                    endpoint: lease.endpoint.clone(),
                    endpoint_lease_epoch: lease.epoch.clone(),
                    event_queue: granted_queue(session.session().event_queue()),
                });
            let response = HelloResponse::new(version, config.peer.clone(), lease, &granted);
            (
                Some(Established::Data {
                    session,
                    version,
                    keepalive,
                }),
                Some(Frame::HelloResponse(response)),
            )
        }
        AuthorityDomain::Admin => {
            let port = match binding.admin(outcome.granted_admin.clone()).await {
                Ok(port) => port,
                Err(code) => return close(code),
            };
            let response = HelloResponse::new(version, config.peer.clone(), None, &outcome);
            (
                Some(Established::Admin {
                    port,
                    version,
                    keepalive,
                }),
                Some(Frame::HelloResponse(response)),
            )
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]
    use super::*;
    use crate::fake::{Fake, peer};
    use tokio::io::AsyncWriteExt as _;

    fn config() -> ServerConfig {
        ServerConfig {
            peer: peer(),
            limits: Limits::default(),
            keepalive: KeepalivePolicy::default(),
            shutdown_grace: Duration::from_secs(5),
            command_deadline: Duration::from_secs(10),
            write_stall: WRITE_STALL,
        }
    }

    /// Feed `bodies` as the client, run the hello phase on `domain`, and
    /// return what it established and every frame it wrote.
    async fn run(domain: AuthorityDomain, bodies: &[&str], fake: &Fake) -> (bool, Vec<Frame>) {
        let (mut client, server) = tokio::io::duplex(1 << 16);
        for body in bodies {
            let frame = interweave_ipc_protocol::encode_frame(body).expect("frame");
            client.write_all(&frame).await.expect("write");
        }
        let mut reader = FrameReader::new(server);
        let (established, reply) = hello_within(
            &mut reader,
            domain,
            fake,
            &config(),
            Duration::from_millis(200),
        )
        .await;
        let established = established.is_some();
        let written: Vec<Frame> = reply.into_iter().collect();
        (established, written)
    }

    fn close_code(frames: &[Frame]) -> Option<TransportError> {
        match frames {
            [Frame::Close(close)] => Some(close.code),
            _ => None,
        }
    }

    const DATA_HELLO: &str = r#"{"type":"hello","ipc_version":{"major":2,"minor":7},
        "client":{"kind":"human-client"},"endpoint":{"id":"human"},
        "requested_capabilities":["events","commands"],"features":["keepalive"]}"#;

    #[tokio::test]
    async fn a_data_hello_claims_its_lease_and_is_answered_with_what_was_granted() {
        let fake = Fake::default();
        let (established, frames) = run(AuthorityDomain::Data, &[DATA_HELLO], &fake).await;
        assert!(established);
        let [Frame::HelloResponse(response)] = &frames[..] else {
            panic!("one hello_response: {frames:?}")
        };
        assert_eq!(
            response.ipc_version,
            IpcVersion {
                major: 2,
                minor: interweave_ipc_protocol::IPC_MAX_MINOR
            },
            "the minor lowered to this build's"
        );
        let lease = response.lease.as_ref().expect("the lease");
        assert_eq!(lease.endpoint.as_str(), "human");
        assert_eq!(
            lease.event_queue.get(),
            4,
            "the session's own bound, which the fake opens with"
        );
        assert_eq!(response.granted_capabilities.len(), 2);
        assert!(
            fake.script().leased.contains("human"),
            "the binding holds it"
        );
    }

    #[tokio::test]
    async fn nothing_but_a_timely_hello_opens_a_connection() {
        let fake = Fake::default();
        let (ok, frames) = run(AuthorityDomain::Data, &[], &fake).await;
        assert!(!ok);
        assert_eq!(
            close_code(&frames),
            Some(TransportError::Timeout),
            "no hello in time"
        );
        let (ok, frames) = run(
            AuthorityDomain::Data,
            &[r#"{"type":"request","id":"1","method":"admin.status"}"#],
            &fake,
        )
        .await;
        assert!(!ok);
        assert_eq!(
            close_code(&frames),
            Some(TransportError::ProtocolViolation),
            "out of phase"
        );
        let (ok, frames) = run(AuthorityDomain::Data, &[r#"{"type":"hello"}"#], &fake).await;
        assert!(!ok);
        assert_eq!(
            close_code(&frames),
            Some(TransportError::ProtocolViolation),
            "malformed"
        );
        assert!(
            fake.script().calls.is_empty(),
            "the binding was never asked"
        );
    }

    #[tokio::test]
    async fn an_unsupported_major_is_answered_with_what_to_speak() {
        let fake = Fake::default();
        let (ok, frames) = run(
            AuthorityDomain::Data,
            &[r#"{"type":"hello","ipc_version":{"major":3,"minor":0},"client":{"kind":"k"}}"#],
            &fake,
        )
        .await;
        assert!(!ok);
        let [Frame::Close(close)] = &frames[..] else {
            panic!("a close: {frames:?}")
        };
        assert_eq!(close.code, TransportError::VersionIncompatible);
        assert_eq!(close.supported.as_deref().map(<[_]>::len), Some(1));
    }

    /// Refused in `hello`, before any lease exists: the binding is never
    /// asked to open anything.
    #[tokio::test]
    async fn a_lease_claim_without_keepalive_is_refused_before_the_binding_is_asked() {
        let fake = Fake::default();
        let no_keepalive = DATA_HELLO.replace(r#","features":["keepalive"]"#, "");
        let (ok, frames) = run(AuthorityDomain::Data, &[&no_keepalive], &fake).await;
        assert!(!ok);
        assert_eq!(close_code(&frames), Some(TransportError::CapabilityDenied));
        assert!(fake.script().calls.is_empty());
        assert!(fake.script().leased.is_empty());
    }

    #[tokio::test]
    async fn the_bindings_lease_refusal_is_the_close_code() {
        let fake = Fake::default();
        for (endpoint, code) in [
            ("nowhere", TransportError::EndpointUnknown),
            ("off", TransportError::EndpointDisabled),
        ] {
            let hello = DATA_HELLO.replace(r#""id":"human""#, &format!(r#""id":"{endpoint}""#));
            let (ok, frames) = run(AuthorityDomain::Data, &[&hello], &fake).await;
            assert!(!ok);
            assert_eq!(close_code(&frames), Some(code), "{endpoint}");
        }
        let (first, _) = run(AuthorityDomain::Data, &[DATA_HELLO], &fake).await;
        assert!(first);
        let (second, frames) = run(AuthorityDomain::Data, &[DATA_HELLO], &fake).await;
        assert!(!second);
        assert_eq!(close_code(&frames), Some(TransportError::EndpointInUse));
    }

    /// The socket is the authority: an admin request on the data socket
    /// and a lease claim on the admin socket are both refused, whatever
    /// the client calls itself.
    #[tokio::test]
    async fn the_socket_decides_the_domain_not_the_frame() {
        let fake = Fake::default();
        let admin_on_data = r#"{"type":"hello","ipc_version":{"major":2,"minor":0},
            "client":{"kind":"transportctl"},"requested_capabilities":["admin.shutdown"]}"#;
        let (ok, frames) = run(AuthorityDomain::Data, &[admin_on_data], &fake).await;
        assert!(!ok);
        assert_eq!(close_code(&frames), Some(TransportError::CapabilityDenied));
        let (ok, frames) = run(AuthorityDomain::Admin, &[admin_on_data], &fake).await;
        assert!(ok, "the same frame on the admin socket: {frames:?}");
        let (ok, frames) = run(AuthorityDomain::Admin, &[DATA_HELLO], &fake).await;
        assert!(!ok);
        assert_eq!(close_code(&frames), Some(TransportError::CapabilityDenied));
        assert!(
            fake.script().leased.is_empty(),
            "the admin socket holds no lease"
        );
    }

    /// `admin.trust` arrived at 2.1: a hello naming it while negotiating
    /// minor 0 is closed `ProtocolViolation` on either socket, before the
    /// socket's own rule is asked; at minor 1 the admin socket grants it
    /// and the data socket refuses it as every `admin.*`. A client
    /// offering a minor above the server's negotiates 1 and is granted it.
    #[tokio::test]
    async fn admin_trust_is_named_only_at_the_minor_that_introduced_it() {
        let hello = |minor: u64| {
            format!(
                r#"{{"type":"hello","ipc_version":{{"major":2,"minor":{minor}}},
                "client":{{"kind":"transportctl"}},
                "requested_capabilities":["admin.status","admin.trust"]}}"#
            )
        };
        let fake = Fake::default();
        for domain in [AuthorityDomain::Admin, AuthorityDomain::Data] {
            let (ok, frames) = run(domain, &[&hello(0)], &fake).await;
            assert!(!ok, "{domain:?}");
            assert_eq!(
                close_code(&frames),
                Some(TransportError::ProtocolViolation),
                "{domain:?}"
            );
        }
        let (ok, frames) = run(AuthorityDomain::Data, &[&hello(1)], &fake).await;
        assert!(!ok);
        assert_eq!(close_code(&frames), Some(TransportError::CapabilityDenied));
        for minor in [1, 7] {
            let (ok, frames) = run(AuthorityDomain::Admin, &[&hello(minor)], &fake).await;
            assert!(ok, "minor {minor}: {frames:?}");
            let [Frame::HelloResponse(response)] = frames.as_slice() else {
                panic!("a response: {frames:?}")
            };
            assert_eq!(
                response.ipc_version.minor,
                minor.min(interweave_ipc_protocol::IPC_MAX_MINOR)
            );
            assert!(
                response
                    .granted_capabilities
                    .contains(&interweave_ipc_protocol::RequestedCapability::AdminTrust)
            );
        }
        // The control: the same hello without it is a 2.0 hello.
        let plain = r#"{"type":"hello","ipc_version":{"major":2,"minor":0},
            "client":{"kind":"transportctl"},"requested_capabilities":["admin.status"]}"#;
        let (ok, frames) = run(AuthorityDomain::Admin, &[plain], &fake).await;
        assert!(ok, "{frames:?}");
    }
}
