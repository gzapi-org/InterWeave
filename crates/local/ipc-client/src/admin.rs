// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The administrative port over the daemon's admin socket.

use std::collections::BTreeSet;
use std::time::Duration;

use interweave_ipc_protocol::{
    AdminStatusResult, EmptyResult, EndpointList, EndpointParams, MAX_SHUTDOWN_GRACE_MS, Request,
    RequestedCapability, SetDefaultParams, SetEnabledParams, SetEnabledResult, ShutdownParams,
    TrustList, TrustListParams, TrustSetParams,
};
use interweave_local_client_api::{
    AdminBinding, AdminCapability, AdminPort, AdminStatus, EndpointAdminView, Generation,
    LocalAdminPort, TrustAdminView,
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

impl IpcBinding {
    /// Open an admin connection asking exactly `asked`.
    async fn admin_hello(
        &self,
        asked: BTreeSet<RequestedCapability>,
    ) -> Result<crate::connection::Opened, TransportError> {
        // No endpoint: an admin connection never holds a lease.
        let hello = hello(self.admin_kind(), None, asked, BTreeSet::new());
        open(&self.paths().admin, hello, |_| None).await
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
        self.learn_minor(Some(minor));
        Ok(minor)
    }
}

impl AdminBinding for IpcBinding {
    type Admin = IpcAdmin;

    /// A capability above 2.0 is named only to a daemon this binding has
    /// learnt selects its minor -- learnt by one probe connection, then
    /// remembered -- and is left out, so the port does not hold it, for a
    /// daemon that does not. A `ProtocolViolation` close to a hello that
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
                let minor = match self.learned_minor() {
                    Some(minor) if !relearnt => minor,
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
            connection: opened.connection,
        })
    }
}

/// An administrative port over the daemon's admin socket. Dropped, its
/// connection ends.
pub struct IpcAdmin {
    port: LocalAdminPort,
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

    /// Read page by page and joined: the local peer from the first page,
    /// the rows in order.
    async fn trust(&self) -> Result<TrustAdminView, TransportError> {
        let pages = self.trust_pages().await?;
        let local_peer = pages.first().and_then(|page| page.local_peer.clone());
        let allowed = pages
            .into_iter()
            .flat_map(|page| page.allowed.into_iter().map(|row| row.peer))
            .collect();
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
