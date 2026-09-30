// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The administrative port over the daemon's admin socket.

use std::collections::BTreeSet;
use std::time::Duration;

use interweave_ipc_protocol::{
    AdminStatusResult, EmptyResult, EndpointList, EndpointParams, MAX_SHUTDOWN_GRACE_MS, Request,
    RequestedCapability, SetDefaultParams, SetEnabledParams, SetEnabledResult, ShutdownParams,
};
use interweave_local_client_api::{
    AdminBinding, AdminCapability, AdminPort, AdminStatus, EndpointAdminView, Generation,
    LocalAdminPort,
};
use interweave_transport_api::{EndpointId, TransportError};

use crate::connection::{Connection, open};
use crate::session::{IpcBinding, hello, mint};

const fn requested(capability: AdminCapability) -> RequestedCapability {
    match capability {
        AdminCapability::Status => RequestedCapability::AdminStatus,
        AdminCapability::Endpoints => RequestedCapability::AdminEndpoints,
        AdminCapability::Shutdown => RequestedCapability::AdminShutdown,
    }
}

impl AdminBinding for IpcBinding {
    type Admin = IpcAdmin;

    async fn admin(
        &self,
        capabilities: BTreeSet<AdminCapability>,
    ) -> Result<IpcAdmin, TransportError> {
        // No endpoint: an admin connection never holds a lease.
        let hello = hello(
            self.admin_kind(),
            None,
            capabilities.into_iter().map(requested).collect(),
            BTreeSet::new(),
        );
        let opened = open(&self.paths().admin, hello, |_| None).await?;
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

impl AdminPort for IpcAdmin {
    fn port(&self) -> &LocalAdminPort {
        &self.port
    }

    async fn status(&self) -> Result<AdminStatus, TransportError> {
        self.connection
            .call::<AdminStatusResult>(Request::AdminStatus)
            .await
            .map(AdminStatus::from)
    }

    async fn leases(&self) -> Result<Vec<EndpointAdminView>, TransportError> {
        self.connection
            .call::<EndpointList>(Request::AdminEndpointsList)
            .await
            .map(|list| {
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
        // The wire carries at most MAX_SHUTDOWN_GRACE_MS; a longer grace is
        // that ceiling, not a refusal.
        let grace_ms = u32::try_from(grace.as_millis())
            .unwrap_or(u32::MAX)
            .min(MAX_SHUTDOWN_GRACE_MS);
        self.connection
            .call::<EmptyResult>(Request::AdminShutdown(ShutdownParams {
                grace_ms: Some(grace_ms),
            }))
            .await
            .map(|_| ())
    }
}
