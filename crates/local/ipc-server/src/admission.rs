// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! Who may connect, and how many (`LOCAL-IPC.md` §Multiple clients; plan
//! §16 (6)).
//!
//! Two checks, both before `hello`:
//!
//! - the PEER CREDENTIAL: the connecting process's uid must be the run
//!   directory's owner, a MUST on Unix. A mismatch is closed without a
//!   word -- nothing is written to a process of another uid -- and
//!   counted;
//! - the SLOTS: at most `max_clients` connections across both sockets, of
//!   which at most `max_admin_clients` on the admin socket. The limit
//!   counts connections, not applications.

// The connection loop reads these; until it lands, the expectation fails
// the build the moment it is met, so it cannot outlive its reason.
#![cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "read by the connection loop, a later commit of this batch"
    )
)]

use std::sync::{Arc, Mutex, PoisonError};

use interweave_ipc_protocol::AuthorityDomain;

use crate::counters::Counters;

/// The client ceilings (`ipc.max_clients`, `ipc.max_admin_clients`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Connections across both sockets (profile default 16).
    pub max_clients: usize,
    /// Of those, admin-socket connections (profile default 4).
    pub max_admin_clients: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_clients: 16,
            max_admin_clients: 4,
        }
    }
}

/// Why a connection was turned away before `hello`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Refusal {
    /// Another uid, or no credential at all: closed without a word.
    PeerCredential,
    /// Every slot of its kind is held: answered `close{Overloaded}`.
    Full,
}

/// The slots every connection draws from.
#[derive(Debug)]
pub(crate) struct Slots {
    limits: Limits,
    held: Mutex<(usize, usize)>,
    counters: Arc<Counters>,
}

/// One held slot: released, and the open-connection count lowered, when
/// the connection's task drops it -- however it ends.
#[derive(Debug)]
pub(crate) struct Slot {
    slots: Arc<Slots>,
    domain: AuthorityDomain,
}

impl Slots {
    pub(crate) fn new(limits: Limits, counters: Arc<Counters>) -> Arc<Self> {
        Arc::new(Self {
            limits,
            held: Mutex::new((0, 0)),
            counters,
        })
    }

    /// Admit a connection whose peer has `peer_uid` (`None` when the socket
    /// could not report one), on `domain`, for a run directory owned by
    /// `owner_uid`.
    pub(crate) fn admit(
        self: &Arc<Self>,
        peer_uid: Option<u32>,
        owner_uid: u32,
        domain: AuthorityDomain,
    ) -> Result<Slot, Refusal> {
        if peer_uid != Some(owner_uid) {
            self.counters.peer_credential_refused();
            return Err(Refusal::PeerCredential);
        }
        let mut held = self.held.lock().unwrap_or_else(PoisonError::into_inner);
        let (total, admin) = &mut *held;
        let admin_full =
            domain == AuthorityDomain::Admin && *admin >= self.limits.max_admin_clients;
        if *total >= self.limits.max_clients || admin_full {
            return Err(Refusal::Full);
        }
        *total += 1;
        if domain == AuthorityDomain::Admin {
            *admin += 1;
        }
        self.counters.opened(domain);
        Ok(Slot {
            slots: Arc::clone(self),
            domain,
        })
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        let mut held = self
            .slots
            .held
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let (total, admin) = &mut *held;
        *total = total.saturating_sub(1);
        if self.domain == AuthorityDomain::Admin {
            *admin = admin.saturating_sub(1);
        }
        self.slots.counters.closed(self.domain);
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]
    use super::*;

    const OWNER: u32 = 1000;

    fn slots(max_clients: usize, max_admin_clients: usize) -> (Arc<Slots>, Arc<Counters>) {
        let counters = Arc::new(Counters::default());
        let limits = Limits {
            max_clients,
            max_admin_clients,
        };
        (Slots::new(limits, Arc::clone(&counters)), counters)
    }

    /// A foreign uid cannot be made unprivileged, so it is injected: the
    /// owner's uid is admitted, one other is refused and counted, and so
    /// is a peer whose credential could not be read.
    #[test]
    fn only_the_run_directorys_owner_is_admitted() {
        let (slots, counters) = slots(16, 4);
        let _ok = slots
            .admit(Some(OWNER), OWNER, AuthorityDomain::Data)
            .expect("the owner");
        assert_eq!(
            slots
                .admit(Some(OWNER + 1), OWNER, AuthorityDomain::Data)
                .err(),
            Some(Refusal::PeerCredential)
        );
        assert_eq!(
            slots.admit(None, OWNER, AuthorityDomain::Admin).err(),
            Some(Refusal::PeerCredential)
        );
        let snapshot = counters.snapshot();
        assert_eq!(snapshot.peer_credential_refused_total, 2);
        assert_eq!(snapshot.data_connections, 1, "a refusal holds no slot");
    }

    #[test]
    fn the_total_counts_both_sockets_and_the_admin_sublimit_counts_its_own() {
        let (slots, counters) = slots(3, 1);
        let admin = slots
            .admit(Some(OWNER), OWNER, AuthorityDomain::Admin)
            .expect("one admin");
        assert_eq!(
            slots
                .admit(Some(OWNER), OWNER, AuthorityDomain::Admin)
                .err(),
            Some(Refusal::Full),
            "the admin sublimit"
        );
        let data = [
            slots
                .admit(Some(OWNER), OWNER, AuthorityDomain::Data)
                .expect("data 1"),
            slots
                .admit(Some(OWNER), OWNER, AuthorityDomain::Data)
                .expect("data 2"),
        ];
        assert_eq!(
            slots.admit(Some(OWNER), OWNER, AuthorityDomain::Data).err(),
            Some(Refusal::Full),
            "the total counts the admin connection too"
        );
        assert_eq!(
            (
                counters.snapshot().data_connections,
                counters.snapshot().admin_connections
            ),
            (2, 1)
        );
        drop(admin);
        let _again = slots
            .admit(Some(OWNER), OWNER, AuthorityDomain::Admin)
            .expect("a released slot is free again");
        drop(data);
        assert_eq!(counters.snapshot().data_connections, 0);
        assert_eq!(counters.snapshot().admin_connections, 1);
    }
}
