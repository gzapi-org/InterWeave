// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! Reply-token lifecycle.
//!
//! A reply token is a **local routing handle**, not a capability. It
//! records where a message came from so a reply can go back the same way,
//! and it confers nothing: current trust and endpoint policy are applied
//! to the reply exactly as to any other send.
//!
//! # Binding to the lease epoch is the whole mechanism
//!
//! A token stores the `local_lease_epoch` it was minted under. After a
//! reconnect the epoch is new, so every token from the previous session
//! stops resolving — [`ReplyResolution::StaleLease`]. Without that a
//! reply issued after reconnect would be routed by a token whose local
//! endpoint is now owned by a different session, delivering it as
//! somebody else.
//!
//! # A token never widens its own route
//!
//! [`ReplyRoute::Direct`] names one remote endpoint and one local
//! endpoint. There is no fallback to the remote default and no
//! substitution of another local endpoint: if the route it names is
//! unavailable, the reply fails rather than going somewhere adjacent.

use std::collections::{BTreeMap, VecDeque};

use interweave_local_client_api::Generation;
use interweave_transport_api::{ChannelId, EndpointId, TransportError, TransportIdentity};

/// Default token lifetime.
pub const DEFAULT_TTL_MS: u64 = 30 * 60 * 1000;
/// Default maximum live tokens per bridge process.
pub const DEFAULT_MAX_TOKENS: usize = 2048;

/// A token was minted twice.
///
/// Not a recoverable condition to retry blindly: the caller must produce
/// a fresh value. Silently overwriting would change what an outstanding
/// handle resolves to, and an opaque token whose meaning can change is
/// not opaque.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DuplicateToken {
    /// The token that was already live.
    pub token: String,
}

impl core::fmt::Display for DuplicateToken {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "reply token {:?} is already live", self.token)
    }
}

impl core::error::Error for DuplicateToken {}

/// The route a token restores.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReplyRoute {
    /// Reply to the exact peer and endpoint a direct message came from.
    Direct {
        /// The authenticated remote peer.
        remote_peer: TransportIdentity,
        /// The remote endpoint that sent it — the reply's destination.
        remote_endpoint: EndpointId,
        /// The local endpoint that received it.
        local_endpoint: EndpointId,
        /// The lease epoch under which the token was minted.
        local_lease_epoch: Generation,
    },
    /// Reply to the channel a broadcast came from.
    ///
    /// Carries no endpoint: broadcast origin is PeerId-only, and a
    /// channel is not a route to a person (ADR-0030).
    Broadcast {
        /// The logical channel.
        channel: ChannelId,
    },
}

/// What resolving a token produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReplyResolution {
    /// The token is live and names this route.
    Route(ReplyRoute),
    /// No such token, or it expired.
    ///
    /// Unknown and expired are one answer on purpose: distinguishing them
    /// would tell a caller whether a token had ever existed.
    Unknown,
    /// The token predates the current lease epoch.
    StaleLease,
    /// The bridge has left the channel this broadcast token names.
    ChannelNotJoined,
}

impl ReplyResolution {
    /// The local error a caller reports for a non-route outcome.
    #[must_use]
    pub const fn as_error(&self) -> Option<TransportError> {
        match self {
            Self::Route(_) => None,
            // An expired or unknown token is a caller argument that no
            // longer means anything, not an authorization failure.
            Self::Unknown | Self::StaleLease => Some(TransportError::InvalidArgument),
            Self::ChannelNotJoined => Some(TransportError::ChannelNotJoined),
        }
    }
}

/// A bounded, short-lived table of reply tokens.
///
/// Not serializable and not `Clone`: tokens disappear on process restart
/// by design, and a type that could be written down invites reviving a
/// route whose lease is long gone.
#[derive(Debug)]
pub struct ReplyTokenTable {
    tokens: BTreeMap<String, (ReplyRoute, u64)>,
    order: VecDeque<String>,
    ttl_ms: u64,
    max_tokens: usize,
}

impl Default for ReplyTokenTable {
    fn default() -> Self {
        Self::new(DEFAULT_TTL_MS, DEFAULT_MAX_TOKENS)
    }
}

impl ReplyTokenTable {
    /// Build a table with explicit bounds.
    #[must_use]
    pub fn new(ttl_ms: u64, max_tokens: usize) -> Self {
        Self {
            tokens: BTreeMap::new(),
            order: VecDeque::new(),
            ttl_ms,
            max_tokens: max_tokens.max(1),
        }
    }

    /// Live tokens.
    #[must_use]
    pub fn len(&self) -> usize {
        self.tokens.len()
    }

    /// Whether the table is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.tokens.is_empty()
    }

    /// Store a route under a caller-supplied opaque token.
    ///
    /// The token value is generated by the caller because unguessability
    /// needs a CSPRNG, which a pure module has no business owning. This
    /// module bounds and expires them; it does not invent them.
    pub fn mint(
        &mut self,
        token: impl Into<String>,
        route: ReplyRoute,
        now_ms: u64,
    ) -> Result<(), DuplicateToken> {
        self.expire(now_ms);
        let token = token.into();

        // REFUSED, not replaced. Callers mint from a CSPRNG, so a
        // collision is either an exhausted generator or a caller reusing
        // a value — neither is a thing to paper over by silently changing
        // what an outstanding opaque handle means. Replacing would also
        // leave the earlier occurrence in `order`, so the next eviction
        // would drop the freshly minted route through a stale queue
        // entry and evict a token that was just created.
        if self.tokens.contains_key(&token) {
            return Err(DuplicateToken { token });
        }

        while self.tokens.len() >= self.max_tokens {
            let Some(oldest) = self.order.pop_front() else {
                break;
            };
            self.tokens.remove(&oldest);
        }

        self.order.push_back(token.clone());
        self.tokens.insert(token, (route, now_ms));
        Ok(())
    }

    /// Resolve a token against the lease epoch currently held.
    ///
    /// `current_epoch` is the epoch the caller owns **now**. Passing the
    /// token's own epoch back in would make the check vacuous, which is
    /// why the parameter is the live one rather than something read from
    /// the token.
    pub fn resolve(
        &mut self,
        token: &str,
        current_epoch: Option<&Generation>,
        is_joined: &dyn Fn(&ChannelId) -> bool,
        now_ms: u64,
    ) -> ReplyResolution {
        self.expire(now_ms);
        let Some((route, _)) = self.tokens.get(token) else {
            return ReplyResolution::Unknown;
        };
        match route {
            ReplyRoute::Direct {
                local_lease_epoch, ..
            } => {
                if current_epoch != Some(local_lease_epoch) {
                    // A reconnect minted a new epoch; this token's local
                    // endpoint may now belong to a different session.
                    return ReplyResolution::StaleLease;
                }
                ReplyResolution::Route(route.clone())
            }
            ReplyRoute::Broadcast { channel } => {
                if is_joined(channel) {
                    ReplyResolution::Route(route.clone())
                } else {
                    // A token does not recreate a subscription.
                    ReplyResolution::ChannelNotJoined
                }
            }
        }
    }

    /// Drop tokens past their TTL.
    pub fn expire(&mut self, now_ms: u64) {
        let ttl = self.ttl_ms;
        let dead: Vec<String> = self
            .tokens
            .iter()
            .filter(|(_, (_, minted))| now_ms.saturating_sub(*minted) >= ttl)
            .map(|(t, _)| t.clone())
            .collect();
        for t in dead {
            self.tokens.remove(&t);
            if let Some(pos) = self.order.iter().position(|o| o == &t) {
                self.order.remove(pos);
            }
        }
    }

    /// Drop every token, as a process restart would.
    pub fn clear(&mut self) {
        self.tokens.clear();
        self.order.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const P1: &str = "12D3KooWDpJ7As7BWAwRMfu1VU2WCqNjvq387JEYKDBj4kx6nXTN";

    fn peer() -> TransportIdentity {
        TransportIdentity::parse(P1).expect("valid identity")
    }
    fn ep(n: &str) -> EndpointId {
        EndpointId::parse(n).expect("valid endpoint")
    }
    fn epoch(seed: &str) -> Generation {
        Generation::parse(format!("{seed:_<16}")).expect("valid generation")
    }
    fn channel() -> ChannelId {
        ChannelId::parse("general").expect("valid channel")
    }
    fn joined(_: &ChannelId) -> bool {
        true
    }
    fn left(_: &ChannelId) -> bool {
        false
    }

    fn direct_route(e: &str) -> ReplyRoute {
        ReplyRoute::Direct {
            remote_peer: peer(),
            remote_endpoint: ep("claude"),
            local_endpoint: ep("human"),
            local_lease_epoch: epoch(e),
        }
    }

    #[test]
    fn a_direct_token_restores_the_exact_route() {
        let mut t = ReplyTokenTable::default();
        t.mint("tok", direct_route("e1"), 0).expect("fresh token");
        let resolved = t.resolve("tok", Some(&epoch("e1")), &joined, 10);
        assert_eq!(resolved, ReplyResolution::Route(direct_route("e1")));
        // The destination is the ORIGINAL remote source endpoint, never a
        // fallback to the remote default.
        if let ReplyResolution::Route(ReplyRoute::Direct {
            remote_endpoint, ..
        }) = resolved
        {
            assert_eq!(remote_endpoint, ep("claude"));
        } else {
            panic!("expected a direct route");
        }
    }

    #[test]
    fn a_token_from_a_previous_lease_epoch_is_stale() {
        // The mechanism: after a reconnect the local endpoint may belong
        // to a different session, so replying by the old token would
        // deliver as somebody else.
        let mut t = ReplyTokenTable::default();
        t.mint("tok", direct_route("old"), 0).expect("fresh token");
        assert_eq!(
            t.resolve("tok", Some(&epoch("new")), &joined, 10),
            ReplyResolution::StaleLease
        );
        // And holding no lease at all is equally stale.
        assert_eq!(
            t.resolve("tok", None, &joined, 10),
            ReplyResolution::StaleLease
        );
    }

    #[test]
    fn unknown_and_expired_are_one_answer() {
        // Distinguishing them would say whether a token ever existed.
        let mut t = ReplyTokenTable::new(1_000, 16);
        assert_eq!(
            t.resolve("never-minted", Some(&epoch("e")), &joined, 0),
            ReplyResolution::Unknown
        );
        t.mint("tok", direct_route("e"), 0).expect("fresh token");
        assert_eq!(
            t.resolve("tok", Some(&epoch("e")), &joined, 1_000),
            ReplyResolution::Unknown
        );
    }

    #[test]
    fn a_broadcast_token_does_not_recreate_a_subscription() {
        let mut t = ReplyTokenTable::default();
        t.mint("b", ReplyRoute::Broadcast { channel: channel() }, 0)
            .expect("fresh token");
        assert!(matches!(
            t.resolve("b", None, &joined, 10),
            ReplyResolution::Route(ReplyRoute::Broadcast { .. })
        ));
        // Left the channel: the token is not a way back in.
        assert_eq!(
            t.resolve("b", None, &left, 10),
            ReplyResolution::ChannelNotJoined
        );
    }

    #[test]
    fn a_broadcast_token_needs_no_lease_epoch() {
        // Broadcast origin is PeerId-only; there is no endpoint to bind.
        let mut t = ReplyTokenTable::default();
        t.mint("b", ReplyRoute::Broadcast { channel: channel() }, 0)
            .expect("fresh token");
        assert!(matches!(
            t.resolve("b", None, &joined, 0),
            ReplyResolution::Route(_)
        ));
    }

    #[test]
    fn the_table_is_bounded_and_evicts_oldest_first() {
        let mut t = ReplyTokenTable::new(DEFAULT_TTL_MS, 2);
        t.mint("a", direct_route("e"), 0).expect("fresh token");
        t.mint("b", direct_route("e"), 1).expect("fresh token");
        t.mint("c", direct_route("e"), 2).expect("fresh token");
        assert_eq!(t.len(), 2);
        assert_eq!(
            t.resolve("a", Some(&epoch("e")), &joined, 3),
            ReplyResolution::Unknown
        );
        assert!(matches!(
            t.resolve("c", Some(&epoch("e")), &joined, 3),
            ReplyResolution::Route(_)
        ));
    }

    #[test]
    fn a_restart_drops_every_token() {
        let mut t = ReplyTokenTable::default();
        t.mint("tok", direct_route("e"), 0).expect("fresh token");
        t.clear();
        assert!(t.is_empty());
        assert_eq!(
            t.resolve("tok", Some(&epoch("e")), &joined, 0),
            ReplyResolution::Unknown
        );
    }

    #[test]
    fn non_route_outcomes_map_to_local_errors() {
        assert_eq!(
            ReplyResolution::Unknown.as_error(),
            Some(TransportError::InvalidArgument)
        );
        assert_eq!(
            ReplyResolution::StaleLease.as_error(),
            Some(TransportError::InvalidArgument)
        );
        assert_eq!(
            ReplyResolution::ChannelNotJoined.as_error(),
            Some(TransportError::ChannelNotJoined)
        );
        assert_eq!(ReplyResolution::Route(direct_route("e")).as_error(), None);
    }
    #[test]
    fn minting_the_same_token_twice_is_refused_and_changes_nothing() {
        // A collision means an exhausted CSPRNG or a caller reusing a
        // value. Overwriting would silently change what an outstanding
        // opaque handle resolves to.
        let mut t = ReplyTokenTable::new(DEFAULT_TTL_MS, 8);
        t.mint("tok", direct_route("first"), 0)
            .expect("fresh token");
        assert_eq!(
            t.mint("tok", direct_route("second"), 1),
            Err(DuplicateToken {
                token: "tok".to_owned()
            })
        );

        // The original route survives untouched. The lease epoch is what
        // distinguishes the two routes, and it is also what would have
        // been silently rewritten: resolving under the FIRST epoch still
        // works, which it could not if the second mint had landed.
        assert!(matches!(
            t.resolve("tok", Some(&epoch("first")), &joined, 2),
            ReplyResolution::Route(ReplyRoute::Direct { .. })
        ));
        assert_eq!(
            t.resolve("tok", Some(&epoch("second")), &joined, 2),
            ReplyResolution::StaleLease,
            "the second mint must not have taken effect"
        );
        assert_eq!(t.len(), 1, "a refused mint adds no entry");
    }

    #[test]
    fn a_refused_mint_does_not_corrupt_the_eviction_order() {
        // The reason refusal matters beyond the overwrite: a duplicate
        // pushed a second occurrence into `order` while the map held one
        // entry, so a later eviction popped the stale occurrence and
        // removed a token that had just been re-minted.
        let mut t = ReplyTokenTable::new(DEFAULT_TTL_MS, 2);
        t.mint("a", direct_route("e"), 0).expect("fresh");
        let _ = t.mint("a", direct_route("e"), 1);
        t.mint("b", direct_route("e"), 2).expect("fresh");

        // Filling to the cap must evict "a" — the genuinely oldest — and
        // leave "b" alone. With the duplicate admitted, `order` held
        // ["a", "a", "b"] against two map entries, so this eviction
        // popped the stale "a" and then the live "b".
        t.mint("c", direct_route("e"), 3).expect("fresh");
        assert_eq!(t.len(), 2);
        assert!(matches!(
            t.resolve("b", Some(&epoch("e")), &joined, 4),
            ReplyResolution::Route(_)
        ));
        assert!(matches!(
            t.resolve("c", Some(&epoch("e")), &joined, 4),
            ReplyResolution::Route(_)
        ));
    }
}
