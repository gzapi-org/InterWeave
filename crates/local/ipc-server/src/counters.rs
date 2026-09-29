// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The counters only the server can keep, reported by `admin.status`
//! beside the port's own view (`ipc/admin-status`'s `ipc` block).

// The connection loop reads these; until it lands, the expectation fails
// the build the moment it is met, so it cannot outlive its reason.
#![cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "read by the connection loop, a later commit of this batch"
    )
)]

use std::sync::atomic::{AtomicU64, Ordering};

use interweave_ipc_protocol::{AuthorityDomain, ServerCounters};

/// Shared by every connection task; each field moves on its own.
#[derive(Debug, Default)]
pub struct Counters {
    data_connections: AtomicU64,
    admin_connections: AtomicU64,
    cross_domain_capability_denied: AtomicU64,
    peer_credential_refused: AtomicU64,
    events_dropped: AtomicU64,
}

impl Counters {
    /// What `admin.status` reports.
    #[must_use]
    pub fn snapshot(&self) -> ServerCounters {
        ServerCounters {
            data_connections: self.data_connections.load(Ordering::Relaxed),
            admin_connections: self.admin_connections.load(Ordering::Relaxed),
            cross_domain_capability_denied_total: self
                .cross_domain_capability_denied
                .load(Ordering::Relaxed),
            peer_credential_refused_total: self.peer_credential_refused.load(Ordering::Relaxed),
            events_dropped_total: self.events_dropped.load(Ordering::Relaxed),
        }
    }

    pub(crate) fn peer_credential_refused(&self) {
        self.peer_credential_refused.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn opened(&self, domain: AuthorityDomain) {
        self.open(domain).fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn closed(&self, domain: AuthorityDomain) {
        self.open(domain).fetch_sub(1, Ordering::Relaxed);
    }

    const fn open(&self, domain: AuthorityDomain) -> &AtomicU64 {
        match domain {
            AuthorityDomain::Data => &self.data_connections,
            AuthorityDomain::Admin => &self.admin_connections,
        }
    }
}
