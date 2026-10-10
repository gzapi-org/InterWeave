// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The binding the facade runs on while the platform withholds network
//! access (human-client-android.md, "Runtime permissions and the
//! network-denied state"): no runtime serves it, so every open answers
//! `BackendUnavailable` and no session, lease or admin port ever exists.
//! The facade then keeps the store -- reading and keeping are local
//! writes and go on -- and sends nothing.

use std::collections::BTreeSet;
use std::marker::PhantomData;

use interweave_local_client_api::{
    AdminBinding, AdminCapability, DataSessionBinding, SessionRequest,
};
use interweave_transport_api::TransportError;
use interweave_transport_embedded::EmbeddedHost;

/// A binding with nothing behind it. It borrows the session and admin
/// types of `B` only to name them; it constructs neither.
pub struct Offline<B>(PhantomData<fn() -> B>);

impl<B> Offline<B> {
    /// The binding.
    #[must_use]
    pub const fn new() -> Self {
        Self(PhantomData)
    }
}

/// The offline counterpart of the embedded host's binding, its types
/// taken from [`EmbeddedHost::binding`]'s signature rather than named:
/// that type is the composition crate's, which this crate does not reach.
#[must_use]
pub fn offline_embedded()
-> Offline<impl DataSessionBinding<Session: Send> + AdminBinding<Admin: Send>> {
    fn like<B: DataSessionBinding + AdminBinding>(_: fn(&EmbeddedHost) -> B) -> Offline<B> {
        Offline::new()
    }
    like(EmbeddedHost::binding)
}

impl<B> Default for Offline<B> {
    fn default() -> Self {
        Self::new()
    }
}

impl<B: DataSessionBinding> DataSessionBinding for Offline<B>
where
    B::Session: Send,
{
    type Session = B::Session;

    fn open(
        &self,
        _request: SessionRequest,
    ) -> impl Future<Output = Result<Self::Session, TransportError>> + Send {
        std::future::ready(Err(TransportError::BackendUnavailable))
    }
}

impl<B: AdminBinding> AdminBinding for Offline<B>
where
    B::Admin: Send,
{
    type Admin = B::Admin;

    fn admin(
        &self,
        _capabilities: BTreeSet<AdminCapability>,
    ) -> impl Future<Output = Result<Self::Admin, TransportError>> + Send {
        std::future::ready(Err(TransportError::BackendUnavailable))
    }
}
