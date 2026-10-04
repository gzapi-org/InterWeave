// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! A binding for this crate's unit tests: it records every port call and
//! answers from a script, so each server rule is tested against the port
//! surface alone. The real binding is exercised over sockets by
//! `tests/ipc-v2`.

// Most answers need no await; the traits are async because the real
// binding's are.
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::unused_async_trait_impl
)]

use std::collections::{BTreeSet, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use interweave_local_client_api::{
    AdminBinding, AdminCapability, AdminPort, AdminStatus, DataSessionBinding, DataSessionPort,
    EndpointAdminView, EndpointLease, Generation, LocalAdminPort, LocalDataSession, SessionEvent,
    SessionRequest, TrustAdminView,
};
use interweave_transport_api::{
    BroadcastMessageV1, ChannelId, ConnectivitySummary, DirectDestination, DirectInboundState,
    EndpointDirectoryV1, EndpointId, Health, MessageId, PathReadiness, Payload,
    PreferredPathPolicy, TransportError, TransportIdentity,
};

pub(crate) const PEER: &str = "12D3KooWDpJ7As7BWAwRMfu1VU2WCqNjvq387JEYKDBj4kx6nXTN";

/// What the fake has been asked, and what it will answer.
#[derive(Debug, Default)]
pub(crate) struct Script {
    /// Every port call, in order, as `method arg`.
    pub(crate) calls: Vec<String>,
    /// Events the next `events(max)` hands out, oldest first.
    pub(crate) events: VecDeque<SessionEvent>,
    /// Endpoints currently leased.
    pub(crate) leased: BTreeSet<String>,
    /// Sessions closed.
    pub(crate) closed: usize,
    /// When set, `send_direct` waits for this before answering.
    pub(crate) hold_send: Option<Arc<tokio::sync::Notify>>,
    /// The status `admin.status` reports.
    pub(crate) health: Option<Health>,
    /// Lease epochs minted.
    pub(crate) epochs: u32,
    /// When set, `join` panics: a binding bug the server must survive.
    pub(crate) panic_join: bool,
    /// How long `close` takes before it releases the lease: a slow
    /// binding, so a test can tell whether the server waited for it.
    pub(crate) close_delay: std::time::Duration,
    /// The allowlist `trust` answers, in any order.
    pub(crate) trusted: Vec<TransportIdentity>,
    /// When set, `shutdown` stops the server with it before answering,
    /// as the daemon's port does through its owner.
    pub(crate) stop_on_shutdown: Option<tokio::sync::oneshot::Sender<()>>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct Fake(pub(crate) Arc<Mutex<Script>>);

impl Fake {
    pub(crate) fn script(&self) -> std::sync::MutexGuard<'_, Script> {
        self.0.lock().unwrap()
    }

    fn call(&self, what: String) {
        self.script().calls.push(what);
    }
}

pub(crate) fn peer() -> TransportIdentity {
    TransportIdentity::parse(PEER).expect("peer")
}

pub(crate) fn connectivity() -> ConnectivitySummary {
    ConnectivitySummary {
        direct_inbound: DirectInboundState::Unknown,
        relay_inbound: PathReadiness::Unavailable,
        active_relay_reservations: 0,
        target_relay_reservations: 0,
        active_relayed_peer_paths: 0,
        hole_punch_inflight: 0,
        preferred_path_policy: PreferredPathPolicy::DirectFirst,
        updated_at: 0,
    }
}

impl DataSessionBinding for Fake {
    type Session = FakeSession;

    async fn open(&self, request: SessionRequest) -> Result<FakeSession, TransportError> {
        let lease = match request.endpoint() {
            None => None,
            Some(endpoint) => {
                let mut script = self.script();
                match endpoint.as_str() {
                    "human" | "agent" => {}
                    "off" => return Err(TransportError::EndpointDisabled),
                    _ => return Err(TransportError::EndpointUnknown),
                }
                if !script.leased.insert(endpoint.as_str().to_owned()) {
                    return Err(TransportError::EndpointInUse);
                }
                script.epochs += 1;
                Some(EndpointLease {
                    endpoint: endpoint.clone(),
                    epoch: Generation::parse(format!("epoch{:0>11}", script.epochs))
                        .expect("epoch"),
                })
            }
        };
        let session = LocalDataSession::new(
            Generation::parse("session_000000001").expect("id"),
            request.client_kind(),
            lease,
            request.capabilities().iter().copied(),
            4,
        )
        .map_err(|_| TransportError::InvalidArgument)?;
        self.call(format!("open {}", request.client_kind()));
        Ok(FakeSession {
            fake: self.clone(),
            session,
        })
    }
}

#[derive(Debug)]
pub(crate) struct FakeSession {
    fake: Fake,
    session: LocalDataSession,
}

impl DataSessionPort for FakeSession {
    fn session(&self) -> &LocalDataSession {
        &self.session
    }

    async fn join(&self, channel: ChannelId) -> Result<(), TransportError> {
        assert!(!self.fake.script().panic_join, "a binding bug");
        self.fake.call(format!("join {}", channel.as_str()));
        Ok(())
    }

    async fn leave(&self, channel: ChannelId) -> Result<(), TransportError> {
        self.fake.call(format!("leave {}", channel.as_str()));
        Ok(())
    }

    async fn broadcast(
        &self,
        channel: ChannelId,
        _: BroadcastMessageV1,
    ) -> Result<(), TransportError> {
        self.fake.call(format!("broadcast {}", channel.as_str()));
        Ok(())
    }

    async fn send_direct(
        &self,
        destination: DirectDestination,
        _: MessageId,
        _: Payload,
    ) -> Result<EndpointId, TransportError> {
        self.session.authorize_direct_send()?;
        self.fake.call(format!("send {destination:?}"));
        let hold = self.fake.script().hold_send.clone();
        if let Some(hold) = hold {
            hold.notified().await;
        }
        Ok(EndpointId::parse("remote").expect("endpoint"))
    }

    async fn events(&self, max: usize) -> Result<Vec<SessionEvent>, TransportError> {
        let mut script = self.fake.script();
        let take = max.min(script.events.len());
        Ok(script.events.drain(..take).collect())
    }

    /// The server never waits here -- its pump drains `events` on a
    /// timer -- so the script is simply looked at until it holds one.
    async fn ready(&self) -> Result<(), TransportError> {
        while self.fake.script().events.is_empty() {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        Ok(())
    }

    async fn query_endpoints(
        &self,
        peer: TransportIdentity,
    ) -> Result<EndpointDirectoryV1, TransportError> {
        self.fake.call(format!("query {}", peer.as_str()));
        Ok(EndpointDirectoryV1 {
            generated_at_ms: 0,
            ttl_ms: 1000,
            endpoints: vec![EndpointId::parse("agent").expect("endpoint")],
        })
    }

    async fn close(self) -> Result<(), TransportError> {
        let delay = self.fake.script().close_delay;
        tokio::time::sleep(delay).await;
        let mut script = self.fake.script();
        script.closed += 1;
        if let Some(lease) = self.session.endpoint_lease() {
            script.leased.remove(lease.endpoint.as_str());
        }
        Ok(())
    }
}

impl AdminBinding for Fake {
    type Admin = FakeAdmin;

    async fn admin(
        &self,
        capabilities: BTreeSet<AdminCapability>,
    ) -> Result<FakeAdmin, TransportError> {
        Ok(FakeAdmin {
            fake: self.clone(),
            port: LocalAdminPort::new(
                Generation::parse("port_00000000001").expect("id"),
                capabilities,
            ),
        })
    }
}

#[derive(Debug)]
pub(crate) struct FakeAdmin {
    fake: Fake,
    port: LocalAdminPort,
}

impl AdminPort for FakeAdmin {
    fn port(&self) -> &LocalAdminPort {
        &self.port
    }

    async fn status(&self) -> Result<AdminStatus, TransportError> {
        let health = self.fake.script().health.unwrap_or(Health::Healthy);
        Ok(AdminStatus {
            health,
            peer: peer(),
            connectivity: connectivity(),
            active_leases: self.fake.script().leased.len(),
            pre_auth: None,
            ingress: None,
        })
    }

    async fn leases(&self) -> Result<Vec<EndpointAdminView>, TransportError> {
        self.fake.call("leases".to_owned());
        Ok(Vec::new())
    }

    async fn revoke_endpoint(&self, endpoint: EndpointId) -> Result<(), TransportError> {
        self.fake.call(format!("revoke {}", endpoint.as_str()));
        Ok(())
    }

    async fn set_endpoint_enabled(
        &self,
        endpoint: EndpointId,
        enabled: bool,
    ) -> Result<Option<Generation>, TransportError> {
        self.fake
            .call(format!("set_enabled {} {enabled}", endpoint.as_str()));
        Ok(None)
    }

    async fn set_default_endpoint(
        &self,
        endpoint: Option<EndpointId>,
    ) -> Result<(), TransportError> {
        self.fake.call(format!("set_default {endpoint:?}"));
        Ok(())
    }

    async fn shutdown(&self, grace: Duration) -> Result<(), TransportError> {
        self.fake.call(format!("shutdown {}", grace.as_millis()));
        if let Some(stop) = self.fake.script().stop_on_shutdown.take() {
            let _ = stop.send(());
        }
        Ok(())
    }

    async fn trust(&self) -> Result<TrustAdminView, TransportError> {
        self.fake.call("trust".to_owned());
        Ok(TrustAdminView {
            local_peer: Some(peer()),
            allowed: self.fake.script().trusted.clone(),
        })
    }

    async fn set_trust(
        &self,
        peer: TransportIdentity,
        allowed: bool,
    ) -> Result<(), TransportError> {
        self.fake
            .call(format!("set_trust {} {allowed}", peer.as_str()));
        Ok(())
    }
}
