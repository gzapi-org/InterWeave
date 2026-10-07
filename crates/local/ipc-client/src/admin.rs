// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The administrative port over the daemon's admin socket.

use std::collections::BTreeSet;
use std::time::Duration;

use interweave_ipc_protocol::{
    AdminStatusResult, EmptyResult, EndpointList, EndpointParams, MAX_SHUTDOWN_GRACE_MS, Method,
    PeerList, PeerListParams, Request, RequestedCapability, SetDefaultParams, SetEnabledParams,
    SetEnabledResult, ShutdownParams, TRUST_SOURCE_SINCE_MINOR, TrustList, TrustListParams,
    TrustSetParams,
};
use interweave_local_client_api::{
    AdminBinding, AdminCapability, AdminPort, AdminStatus, EndpointAdminView, Generation,
    LocalAdminPort, PeerGateView, TrustAdminView, TrustedPeer,
};
use interweave_transport_api::{EndpointId, TransportError, TransportIdentity};

use crate::connection::{Connection, open};
use crate::session::{IpcBinding, hello, mint};

const fn requested(capability: AdminCapability) -> RequestedCapability {
    match capability {
        AdminCapability::Status => RequestedCapability::AdminStatus,
        AdminCapability::Endpoints => RequestedCapability::AdminEndpoints,
        AdminCapability::Shutdown => RequestedCapability::AdminShutdown,
        AdminCapability::Trust => RequestedCapability::AdminTrust,
    }
}

/// Pages `trust` reads at most: a full allowlist is four (4096 peers,
/// 1024 a page), and a list changing while it is read may add a page or
/// so. A server past this is not paging, and the read ends `Internal`
/// rather than following it.
const MAX_TRUST_PAGES: usize = 8;

/// Pages `peers` reads at most: a full allowlist is eight (4096 peers,
/// 512 a page), and the gate's rows may move while they are read.
const MAX_PEER_PAGES: usize = 10;

impl IpcBinding {
    /// Open an admin connection asking exactly `asked`.
    async fn admin_hello(
        &self,
        asked: BTreeSet<RequestedCapability>,
    ) -> Result<crate::connection::Opened, TransportError> {
        // No endpoint: an admin connection never holds a lease.
        let hello = hello(self.admin_kind(), None, asked, BTreeSet::new());
        let opened = open(&self.paths().admin, hello, |_| None).await?;
        // Every answer says what the daemon selects now: remembered, so
        // an upgrade a port happens to see is not left unlearnt.
        self.learn_minor(Some(opened.response.ipc_version.minor));
        Ok(opened)
    }

    /// Learn the minor the daemon selects, over a connection asking only
    /// what 2.0 has (`LOCAL-IPC.md` §Version negotiation: a first hello
    /// names only 2.0 capabilities), closed once answered.
    async fn probe_minor(
        &self,
        wanted: &BTreeSet<RequestedCapability>,
    ) -> Result<u64, TransportError> {
        let first = wanted
            .iter()
            .copied()
            .filter(|c| c.since_minor() == 0)
            .collect();
        let opened = self.admin_hello(first).await?;
        let minor = opened.response.ipc_version.minor;
        // CLOSED, not dropped, before the real hello: the admin socket's
        // own client ceiling may be one.
        let _ = opened.connection.close().await;
        Ok(minor)
    }
}

impl AdminBinding for IpcBinding {
    type Admin = IpcAdmin;

    /// A capability above 2.0 is named only to a daemon this binding has
    /// learnt selects its minor -- learnt by a probe connection, then
    /// remembered -- and is left out, so the port does not hold it, for a
    /// daemon that does not; a minor remembered below it is probed again
    /// at the next port, so an upgraded daemon is asked
    /// (`a_daemon_upgraded_since_the_probe_is_asked_again`). A `ProtocolViolation` close to a hello that
    /// named one means the daemon changed (a restart may change its
    /// minor): the minor is learnt again and the hello sent once more
    /// (`a_trust_port_probes_once_and_relearns_after_a_refusal`).
    async fn admin(
        &self,
        capabilities: BTreeSet<AdminCapability>,
    ) -> Result<IpcAdmin, TransportError> {
        let wanted: BTreeSet<RequestedCapability> =
            capabilities.into_iter().map(requested).collect();
        let needs = wanted.iter().map(|c| c.since_minor()).max().unwrap_or(0);
        let opened = if needs == 0 {
            self.admin_hello(wanted).await?
        } else {
            let mut relearnt = false;
            loop {
                // A minor remembered below what is wanted is asked again: the
                // daemon may have been upgraded since, and only a refusal
                // would otherwise ever clear it.
                let minor = match self.learned_minor() {
                    Some(minor) if !relearnt && minor >= needs => minor,
                    _ => self.probe_minor(&wanted).await?,
                };
                let asked = wanted
                    .iter()
                    .copied()
                    .filter(|c| c.since_minor() <= minor)
                    .collect();
                match self.admin_hello(asked).await {
                    Err(TransportError::ProtocolViolation) if !relearnt => {
                        self.learn_minor(None);
                        relearnt = true;
                    }
                    other => break other?,
                }
            }
        };
        let granted = opened
            .response
            .granted_capabilities
            .iter()
            .filter_map(|c| c.as_admin());
        Ok(IpcAdmin {
            port: LocalAdminPort::new(mint(), granted),
            minor: opened.response.ipc_version.minor,
            connection: opened.connection,
        })
    }
}

/// An administrative port over the daemon's admin socket. Dropped, its
/// connection ends.
pub struct IpcAdmin {
    port: LocalAdminPort,
    /// The minor this port's connection negotiated: a method above it is
    /// not sent (`admin.peers.list`, 2.2, under the 2.0 `admin.status`).
    minor: u64,
    connection: Connection,
}

impl std::fmt::Debug for IpcAdmin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IpcAdmin")
            .field("port", &self.port)
            .finish_non_exhaustive()
    }
}

impl IpcAdmin {
    /// `admin.status`'s result object as the daemon sent it, the IPC
    /// server's counters included -- which [`AdminPort::status`]'s neutral
    /// view has no field for. What `transportctl status --json` prints
    /// (plan §16 (11)), so its output validates against the method's
    /// schema.
    ///
    /// # Errors
    /// As [`AdminPort::status`].
    pub async fn status_result(&self) -> Result<AdminStatusResult, TransportError> {
        self.connection
            .call::<AdminStatusResult>(Request::AdminStatus)
            .await
    }

    /// `admin.endpoints.list`'s result object as the daemon sent it, for
    /// `transportctl endpoints list --json`.
    ///
    /// # Errors
    /// As [`AdminPort::leases`].
    pub async fn endpoints_result(&self) -> Result<EndpointList, TransportError> {
        self.connection
            .call::<EndpointList>(Request::AdminEndpointsList)
            .await
    }

    /// Every page of `admin.trust.list` as the daemon sent it, in order,
    /// for `transportctl trust list --json`: one `ipc/trust-list` each.
    ///
    /// # Errors
    /// As [`AdminPort::trust`]; `Internal` for a daemon whose cursor does
    /// not advance or that pages past [`MAX_TRUST_PAGES`].
    pub async fn trust_pages(&self) -> Result<Vec<TrustList>, TransportError> {
        let mut pages: Vec<TrustList> = Vec::new();
        let mut after: Option<TransportIdentity> = None;
        loop {
            if pages.len() == MAX_TRUST_PAGES {
                return Err(TransportError::Internal);
            }
            let page = self
                .connection
                .call::<TrustList>(Request::AdminTrustList(TrustListParams {
                    after: after.clone(),
                }))
                .await?;
            let next = page.next.clone();
            pages.push(page);
            match next {
                // A cursor that does not move would read the same page
                // for ever.
                Some(next) if after.as_ref().is_none_or(|after| next > *after) => {
                    after = Some(next);
                }
                Some(_) => return Err(TransportError::Internal),
                None => return Ok(pages),
            }
        }
    }

    /// Every page of `admin.peers.list` as the daemon sent it, in order,
    /// for `transportctl peers list --json`: one `ipc/peer-list` each.
    ///
    /// Not sent on a connection that negotiated below 2.2, where the
    /// daemon would answer it as an unknown method: answered that way
    /// here instead, without a round trip (`LOCAL-IPC.md`,
    /// `admin.peers.list`).
    ///
    /// # Errors
    /// `ProtocolUnsupported` below 2.2; as [`AdminPort::peers`]; and
    /// `Internal` for a daemon whose cursor does not advance or that pages
    /// past [`MAX_PEER_PAGES`].
    pub async fn peers_pages(&self) -> Result<Vec<PeerList>, TransportError> {
        if self.minor < Method::AdminPeersList.entry().since_minor {
            return Err(TransportError::ProtocolUnsupported);
        }
        let mut pages: Vec<PeerList> = Vec::new();
        let mut after: Option<TransportIdentity> = None;
        loop {
            if pages.len() == MAX_PEER_PAGES {
                return Err(TransportError::Internal);
            }
            let page = self
                .connection
                .call::<PeerList>(Request::AdminPeersList(PeerListParams {
                    after: after.clone(),
                }))
                .await?;
            let next = page.next.clone();
            pages.push(page);
            match next {
                Some(next) if after.as_ref().is_none_or(|after| next > *after) => {
                    after = Some(next);
                }
                Some(_) => return Err(TransportError::Internal),
                None => return Ok(pages),
            }
        }
    }

    /// `admin.shutdown`, its grace ABSENT when `None` -- "the daemon's
    /// default" (`ipc/shutdown-params`), which the neutral
    /// [`AdminPort::shutdown`] cannot say, since it always names one. A
    /// grace past the wire's ceiling is that ceiling, not a refusal.
    ///
    /// # Errors
    /// As [`AdminPort::shutdown`].
    pub async fn request_shutdown(&self, grace: Option<Duration>) -> Result<(), TransportError> {
        let grace_ms = grace.map(|grace| {
            u32::try_from(grace.as_millis())
                .unwrap_or(u32::MAX)
                .min(MAX_SHUTDOWN_GRACE_MS)
        });
        self.connection
            .call::<EmptyResult>(Request::AdminShutdown(ShutdownParams { grace_ms }))
            .await
            .map(|_| ())
    }
}

impl AdminPort for IpcAdmin {
    fn port(&self) -> &LocalAdminPort {
        &self.port
    }

    async fn status(&self) -> Result<AdminStatus, TransportError> {
        self.status_result().await.map(AdminStatus::from)
    }

    async fn leases(&self) -> Result<Vec<EndpointAdminView>, TransportError> {
        self.endpoints_result().await.map(|list| {
            list.endpoints
                .into_iter()
                .map(EndpointAdminView::from)
                .collect()
        })
    }

    async fn revoke_endpoint(&self, endpoint: EndpointId) -> Result<(), TransportError> {
        self.connection
            .call::<EmptyResult>(Request::AdminEndpointsRevoke(EndpointParams { endpoint }))
            .await
            .map(|_| ())
    }

    async fn set_endpoint_enabled(
        &self,
        endpoint: EndpointId,
        enabled: bool,
    ) -> Result<Option<Generation>, TransportError> {
        self.connection
            .call::<SetEnabledResult>(Request::AdminEndpointsSetEnabled(SetEnabledParams {
                endpoint,
                enabled,
            }))
            .await
            .map(|result| result.revoked_epoch)
    }

    async fn set_default_endpoint(
        &self,
        endpoint: Option<EndpointId>,
    ) -> Result<(), TransportError> {
        self.connection
            .call::<EmptyResult>(Request::AdminEndpointsSetDefault(SetDefaultParams {
                endpoint,
            }))
            .await
            .map(|_| ())
    }

    async fn shutdown(&self, grace: Duration) -> Result<(), TransportError> {
        self.request_shutdown(Some(grace)).await
    }

    /// Read page by page and joined, the rows in order.
    async fn peers(&self) -> Result<Vec<PeerGateView>, TransportError> {
        Ok(self
            .peers_pages()
            .await?
            .into_iter()
            .flat_map(|page| page.peers)
            .map(|row| PeerGateView {
                peer: row.peer,
                connected: row.connected,
                backoff_until: row.backoff_until,
                quarantined_until: row.quarantined_until,
                last_outcome: row.last_outcome,
            })
            .collect())
    }

    /// Read page by page and joined: the local peer from the first page,
    /// the rows in order.
    ///
    /// Only on a connection that negotiated 2.3 or later, whose row says
    /// where each peer comes from: below it the 2.1 row carries no
    /// source, and an unknown is never shown as a value, so the read is
    /// refused `ProtocolUnsupported` without a round trip, as
    /// [`AdminPort::peers`] is below 2.2 (`LOCAL-IPC.md` `admin.trust`;
    /// architect-cto's ruling, relay seq 15229). `set_trust` carries no
    /// row and is unchanged from 2.1.
    async fn trust(&self) -> Result<TrustAdminView, TransportError> {
        if self.minor < TRUST_SOURCE_SINCE_MINOR {
            return Err(TransportError::ProtocolUnsupported);
        }
        let pages = self.trust_pages().await?;
        let local_peer = pages.first().and_then(|page| page.local_peer.clone());
        // At 2.3 every row a production daemon sends is persisted with its
        // source (a store-less runtime negotiates no minor above 2.2); a
        // row without one is a daemon this client cannot read truthfully.
        let allowed = pages
            .into_iter()
            .flat_map(|page| page.allowed)
            .map(|row| match row.source {
                Some(source) if row.persisted => Ok(TrustedPeer {
                    peer: row.peer,
                    persisted: true,
                    source,
                }),
                _ => Err(TransportError::Internal),
            })
            .collect::<Result<_, _>>()?;
        Ok(TrustAdminView {
            local_peer,
            allowed,
        })
    }

    async fn set_trust(
        &self,
        peer: TransportIdentity,
        allowed: bool,
    ) -> Result<(), TransportError> {
        self.connection
            .call::<EmptyResult>(Request::AdminTrustSet(TrustSetParams { peer, allowed }))
            .await
            .map(|_| ())
    }
}
