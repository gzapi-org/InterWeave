// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The bounded command/event port between the Kademlia provider and the
//! Swarm-owned driver.
//!
//! Kademlia here is **peer routing only** (ADR-0009). Two absences in this
//! module are the substance of that, and both are structural rather than
//! documented:
//!
//! - **No record command exists.** [`KademliaCommand`] has no variant for
//!   `put_record`, `get_record`, `start_providing`, or anything like them.
//!   The rule is not "the driver must not call those" — there is nothing
//!   to call. Any later record use requires a new ADR, and would show up
//!   here as a new variant that a reviewer cannot miss.
//! - **No dial command exists.** Iterative queries make the Swarm request
//!   dials, but those are behaviour-originated and pass the root
//!   `DialAdmissionGate` (ADR-0011). A `Dial` variant on this port would
//!   be a provider-owned dial, which is exactly what the gate exists to
//!   prevent.
//!
//! Nothing here touches libp2p, a Swarm, or a socket. The driver that
//! consumes these commands owns all of that.

#![forbid(unsafe_code)]

use interweave_discovery_api::{CandidatePeer, MAX_ADDRESS_BYTES, MAX_ADDRESSES};
use interweave_transport_api::TransportIdentity;
use serde::{Deserialize, Serialize};

/// Ceiling on results one query may return (`max_results_per_query`).
pub const MAX_RESULTS_PER_QUERY: usize = 20;
/// Ceiling on the routing table (`max_routing_peers`).
pub const MAX_ROUTING_PEERS: usize = 1024;
/// Consecutive no-progress exploration rounds before saturation.
pub const SATURATION_ROUNDS: u32 = 3;
/// Cap on the backed-off exploration interval, in milliseconds.
pub const MAX_EXPLORATION_INTERVAL_MS: u64 = 15 * 60 * 1000;

/// Whether this node serves the DHT or only queries it.
///
/// Set explicitly at start rather than inferred. A client-mode peer is not
/// promised to be discoverable by targeted peer routing, and inferring the
/// mode would make that promise accidentally.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum KademliaMode {
    /// Query only; does not answer routing requests.
    Client,
    /// Serves routing requests as well as querying.
    Server,
}

/// Why a query was issued.
///
/// The class travels with the query because the budgets, the cooldowns and
/// the saturation logic all differ by class, and a driver that lost the
/// distinction would account for them together.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum QueryClass {
    /// Initial, self, or bucket refresh.
    Bootstrap,
    /// Lookup of an independently trusted PeerId with fresh server-capability
    /// evidence and no usable addresses.
    Targeted,
    /// Random-key exploration with bounded results.
    Exploration,
}

/// Read a bounded sequence without materializing it first.
///
/// `Vec::deserialize` parses and allocates the whole array before a
/// length check can run, so a ceiling applied afterwards rejects the
/// RESULT while the input has already been paid for. One element past
/// the limit is enough to know.
fn bounded_seq<'de, D, T>(
    deserializer: D,
    max: usize,
    what: &'static str,
) -> Result<Vec<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    struct Bounded<T> {
        max: usize,
        what: &'static str,
        _item: core::marker::PhantomData<T>,
    }

    impl<'de, T: Deserialize<'de>> serde::de::Visitor<'de> for Bounded<T> {
        type Value = Vec<T>;

        fn expecting(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            write!(f, "at most {} {}", self.max, self.what)
        }

        fn visit_seq<A: serde::de::SeqAccess<'de>>(
            self,
            mut seq: A,
        ) -> Result<Self::Value, A::Error> {
            let mut out = Vec::new();
            while let Some(item) = seq.next_element::<T>()? {
                if out.len() >= self.max {
                    return Err(serde::de::Error::custom(format!(
                        "at most {} {}, got more",
                        self.max, self.what
                    )));
                }
                out.push(item);
            }
            Ok(out)
        }
    }

    deserializer.deserialize_seq(Bounded {
        max,
        what,
        _item: core::marker::PhantomData,
    })
}

/// Why a bounded collection on this port was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PortLimit {
    /// More offered addresses than [`MAX_ADDRESSES`].
    TooManyAddresses {
        /// How many were supplied.
        got: usize,
    },
    /// An offered address longer than [`MAX_ADDRESS_BYTES`], or empty.
    AddressOutOfBounds {
        /// The length supplied.
        got: usize,
    },
    /// More results than [`MAX_RESULTS_PER_QUERY`].
    TooManyResults {
        /// How many were supplied.
        got: usize,
    },
}

impl core::fmt::Display for PortLimit {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::TooManyAddresses { got } => {
                write!(f, "{got} offered addresses exceeds {MAX_ADDRESSES}")
            }
            Self::AddressOutOfBounds { got } => write!(
                f,
                "an offered address is {got} bytes; the limit is 1..={MAX_ADDRESS_BYTES}"
            ),
            Self::TooManyResults { got } => {
                write!(f, "{got} query results exceeds {MAX_RESULTS_PER_QUERY}")
            }
        }
    }
}

impl core::error::Error for PortLimit {}

/// One opaque address on a routing offer, bounded before it is owned.
///
/// # Why the length check is here and not in the collection
///
/// [`OfferedAddresses::new`] used to take `Item = String` and check
/// `len()` in the loop. By then the allocation has already happened:
/// `next()` must finish building the `String` before the loop can look
/// at it, so a single oversized item was attacker-controlled memory
/// spent before the ceiling ever ran.
///
/// [`Self::parse`] takes a `&str` and checks the borrowed slice, so the
/// copy is made only for a value that is going to be kept. That is as
/// early as this crate can act: a caller who has ALREADY allocated an
/// oversized `String` has already paid for it, and no signature here
/// can undo that. The bound has to sit where the bytes are first read,
/// which is why this type exists to be threaded outward rather than
/// constructed from strings at the last moment.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct OfferedAddress(String);

impl OfferedAddress {
    /// Check a borrowed address and take a copy only if it fits.
    ///
    /// # Errors
    /// Returns [`PortLimit::AddressOutOfBounds`] outside
    /// `1..=`[`MAX_ADDRESS_BYTES`].
    pub fn parse(address: &str) -> Result<Self, PortLimit> {
        if address.is_empty() || address.len() > MAX_ADDRESS_BYTES {
            return Err(PortLimit::AddressOutOfBounds { got: address.len() });
        }
        Ok(Self(address.to_owned()))
    }

    /// The address.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl core::fmt::Display for OfferedAddress {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for OfferedAddress {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;

        impl serde::de::Visitor<'_> for V {
            type Value = OfferedAddress;

            fn expecting(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
                write!(f, "an address of 1..={MAX_ADDRESS_BYTES} bytes")
            }

            // `visit_str` rather than deserializing a `String` first:
            // for input the format can hand over borrowed, the length
            // is judged before anything is copied.
            fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Self::Value, E> {
                OfferedAddress::parse(v).map_err(E::custom)
            }
        }

        d.deserialize_str(V)
    }
}

/// The addresses on a routing offer, bounded by construction.
///
/// # Why this is a type and not a `Vec` with a checked deserializer
///
/// The first version of this bound lived in `deserialize_with`, and the
/// test beside it said in as many words that this port is crossed as a
/// Rust value in-process and that its serde impl exists for fixtures
/// and diagnostics. Both statements were true, and together they say
/// the bound was on the path that does not matter: a provider building
/// `OfferRoutingPeer { addresses: vec![..] }` directly -- the runtime
/// path the port is designed for -- never went near it.
///
/// A validated type has no such gap. There is one constructor, the
/// deserializer goes through it, and a `Vec` large enough to matter
/// cannot become one of these by any route.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct OfferedAddresses(Vec<OfferedAddress>);

impl OfferedAddresses {
    /// Build from opaque addresses.
    ///
    /// # Errors
    /// Returns [`PortLimit`] naming the bound that was exceeded.
    pub fn new(addresses: impl IntoIterator<Item = OfferedAddress>) -> Result<Self, PortLimit> {
        // READ AT MOST ONE PAST THE LIMIT. `collect()` first and check
        // the length after is the same defect this crate already fixed
        // on the wire path -- it materializes the whole input before the
        // bound can look, so the ceiling rejects a `Vec` already paid
        // for, and against an unbounded lazy iterator it never reaches
        // the check at all.
        //
        // The per-item bound is not here: it lives in
        // [`OfferedAddress::parse`], because by the time an item is
        // yielded its allocation has happened.
        let mut out = Vec::new();
        for address in addresses {
            if out.len() >= MAX_ADDRESSES {
                return Err(PortLimit::TooManyAddresses {
                    got: MAX_ADDRESSES + 1,
                });
            }
            out.push(address);
        }
        Ok(Self(out))
    }

    /// Parse and bound a sequence of borrowed addresses.
    ///
    /// The convenience most callers want, and the one that keeps the
    /// length check ahead of the copy.
    ///
    /// # Errors
    /// Returns [`PortLimit`] naming the bound that was exceeded.
    pub fn parse_all<'a>(addresses: impl IntoIterator<Item = &'a str>) -> Result<Self, PortLimit> {
        let mut out = Vec::new();
        for address in addresses {
            if out.len() >= MAX_ADDRESSES {
                return Err(PortLimit::TooManyAddresses {
                    got: MAX_ADDRESSES + 1,
                });
            }
            out.push(OfferedAddress::parse(address)?);
        }
        Ok(Self(out))
    }

    /// The addresses.
    #[must_use]
    pub fn as_slice(&self) -> &[OfferedAddress] {
        &self.0
    }

    /// How many.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether there are none.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl<'de> Deserialize<'de> for OfferedAddresses {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        // Counted as it arrives AND then run through `new`, so the wire
        // path stops reading early and still ends up at the one
        // constructor rather than beside it.
        let raw = bounded_seq(d, MAX_ADDRESSES, "offered addresses")?;
        Self::new(raw).map_err(serde::de::Error::custom)
    }
}

/// One query's observed candidates, bounded by construction.
///
/// The doc comment here used to say "bounded by `max_results_per_query`"
/// above a bare `Vec` that nothing checked; then it was bounded only on
/// the deserializer, which the driver does not use. Same reasoning as
/// [`OfferedAddresses`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct ObservedCandidates(Vec<CandidatePeer>);

impl ObservedCandidates {
    /// Build from observed candidates.
    ///
    /// # Errors
    /// Returns [`PortLimit::TooManyResults`] above
    /// [`MAX_RESULTS_PER_QUERY`].
    pub fn new(candidates: impl IntoIterator<Item = CandidatePeer>) -> Result<Self, PortLimit> {
        // One past the limit is enough to know; see [`OfferedAddresses::new`].
        let mut out = Vec::new();
        for candidate in candidates {
            if out.len() >= MAX_RESULTS_PER_QUERY {
                return Err(PortLimit::TooManyResults {
                    got: MAX_RESULTS_PER_QUERY + 1,
                });
            }
            out.push(candidate);
        }
        Ok(Self(out))
    }

    /// The candidates.
    #[must_use]
    pub fn as_slice(&self) -> &[CandidatePeer] {
        &self.0
    }

    /// How many.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether there are none.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl<'de> Deserialize<'de> for ObservedCandidates {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw = bounded_seq(d, MAX_RESULTS_PER_QUERY, "query results")?;
        Self::new(raw).map_err(serde::de::Error::custom)
    }
}

/// A command the provider issues to the driver.
///
/// Deliberately small and deliberately incomplete: see the module note on
/// what is absent and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "command")]
pub enum KademliaCommand {
    /// Set client or server mode. Explicit, never inferred.
    SetMode {
        /// The mode to enter.
        mode: KademliaMode,
    },
    /// Offer a peer for routing-table admission.
    ///
    /// An **offer**, not an insertion. The driver still applies address
    /// checks, trust policy, exact protocol-support evidence, and the
    /// table bounds — a hint never goes directly into the routing table.
    OfferRoutingPeer {
        /// Opaque addresses to try.
        ///
        /// This is the port a remote-influenced hint crosses, and a
        /// `Vec<String>` with no ceiling is the routing table's memory
        /// decided by whoever sent the hint. [`OfferedAddresses`] makes
        /// the ceiling a property of the value rather than of one path
        /// to it.
        addresses: OfferedAddresses,
        /// The peer being offered.
        peer: TransportIdentity,
    },
    /// Start one query of the given class.
    StartQuery {
        /// Which class, for budget and cooldown accounting.
        class: QueryClass,
        /// The 32-byte lookup key.
        ///
        /// A fixed array, so an exploration key cannot be the wrong width
        /// and a "key" cannot smuggle a payload — this is peer routing,
        /// and the key space is the identifier space.
        key: [u8; 32],
    },
    /// Stop new queries and settle in-flight work.
    Shutdown,
}

/// What the driver reports back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "event")]
pub enum KademliaEvent {
    /// A query returned peers, already normalized as discovery candidates.
    QueryResults {
        /// The observed candidates, bounded by their own type.
        candidates: ObservedCandidates,
        /// Which class produced them.
        class: QueryClass,
    },
    /// A peer entered the routing table.
    RoutingPeerAdded {
        /// The admitted peer.
        peer: TransportIdentity,
    },
    /// A peer left the routing table, by eviction or expiry.
    ///
    /// Removes only Kademlia provenance. Whether the aggregate peer
    /// survives is `DiscoveryManager`'s decision, since another provider
    /// may still support it.
    RoutingPeerRemoved {
        /// The departed peer.
        peer: TransportIdentity,
    },
    /// A query ended without results.
    QueryFailed {
        /// Which class.
        class: QueryClass,
        /// Bounded diagnostic class, not a free-form message.
        reason: QueryFailure,
    },
}

/// Why a query produced nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueryFailure {
    /// The query exceeded `query_timeout`.
    TimedOut,
    /// No routing peers were available to query.
    NoRoutingPeers,
    /// A concurrency or rate budget refused it.
    BudgetExhausted,
    /// The driver is shutting down.
    ShuttingDown,
}

/// The routing view the provider reasons about.
///
/// Holds counts, not peers: the provider needs to know whether the view is
/// satisfied, and giving it the membership would invite it to make routing
/// decisions the driver owns.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoutingView {
    /// Peers currently in the routing table.
    pub routing_peers: u32,
    /// Configured `target_routing_peers`.
    pub target_routing_peers: u32,
    /// Configured `max_routing_peers`.
    pub max_routing_peers: u32,
    /// Distinct remote trusted peers, excluding the local identity.
    pub remote_trusted_population: u32,
    /// Consecutive exploration rounds that admitted nothing new.
    pub no_progress_rounds: u32,
}

impl RoutingView {
    /// The runtime target, capped by what trust actually permits.
    ///
    /// `min(target, max, remote_trusted_population)`. The third term is the
    /// one that matters in practice: a profile trusting two peers cannot
    /// reach a target of 64 no matter how long it explores, and a view that
    /// ignored it would chase an unreachable number forever.
    #[must_use]
    pub const fn effective_target(&self) -> u32 {
        let mut t = self.target_routing_peers;
        if self.max_routing_peers < t {
            t = self.max_routing_peers;
        }
        if self.remote_trusted_population < t {
            t = self.remote_trusted_population;
        }
        t
    }

    /// Whether the routing table has reached its effective target.
    #[must_use]
    pub const fn is_target_satisfied(&self) -> bool {
        self.routing_peers >= self.effective_target()
    }

    /// Whether exploration has stopped finding anything.
    ///
    /// Saturation is a legitimate resting state, not a failure: a small
    /// trusted overlay runs out of peers to find, and treating that as
    /// unhealthy would keep a two-peer deployment permanently degraded.
    #[must_use]
    pub const fn is_saturated(&self) -> bool {
        self.no_progress_rounds >= SATURATION_ROUNDS
    }

    /// Whether Kademlia can ever become healthy with this trust population.
    ///
    /// With no remote trusted peers there is nobody to route with, and the
    /// provider reports unavailable after startup grace rather than
    /// retrying into an empty set.
    #[must_use]
    pub const fn can_become_healthy(&self) -> bool {
        self.remote_trusted_population > 0
    }

    /// The health this view implies, given recent query success.
    ///
    /// Healthy when target-satisfied **or** saturated, which is why
    /// saturation is modelled at all.
    #[must_use]
    pub const fn health(
        &self,
        recent_queries_succeeded: bool,
    ) -> interweave_discovery_api::ProviderHealth {
        use interweave_discovery_api::ProviderHealth;
        if !self.can_become_healthy() {
            return ProviderHealth::Unavailable;
        }
        if recent_queries_succeeded && (self.is_target_satisfied() || self.is_saturated()) {
            ProviderHealth::Healthy
        } else {
            ProviderHealth::Degraded
        }
    }

    /// The next exploration interval, backing off while nothing is found.
    ///
    /// Doubles per no-progress round from `base_ms`, capped at 15 minutes.
    /// Without this a two-peer overlay would run a useless 60-second
    /// exploration loop forever.
    #[must_use]
    pub const fn next_exploration_interval_ms(&self, base_ms: u64) -> u64 {
        // Clamp the BASE, not only the backed-off value. The profile
        // permits an exploration base up to one hour, so a view with zero
        // no-progress rounds would otherwise skip the loop entirely and
        // return four times the cap this function promises.
        let mut interval = if base_ms > MAX_EXPLORATION_INTERVAL_MS {
            MAX_EXPLORATION_INTERVAL_MS
        } else {
            base_ms
        };
        let mut round = 0;
        while round < self.no_progress_rounds {
            interval = interval.saturating_mul(2);
            if interval >= MAX_EXPLORATION_INTERVAL_MS {
                return MAX_EXPLORATION_INTERVAL_MS;
            }
            round += 1;
        }
        interval
    }
}

/// The cross-field rules the configuration must satisfy when enabled.
///
/// Stated as code beside the port that depends on them, because these are
/// hard validation errors rather than warnings and the driver's bounds
/// assume they already hold.
///
/// # Errors
/// Returns the first violated rule.
pub fn validate_limits(
    target_routing_peers: u32,
    max_routing_peers: u32,
    bootstrap_min_interval_ms: u64,
    bootstrap_refresh_interval_ms: u64,
    max_results_per_query: u32,
    kbucket_size: u32,
) -> Result<(), LimitViolation> {
    if target_routing_peers > max_routing_peers {
        return Err(LimitViolation::TargetAboveMax);
    }
    if bootstrap_refresh_interval_ms < bootstrap_min_interval_ms {
        return Err(LimitViolation::RefreshBelowMinimum);
    }
    if max_results_per_query > kbucket_size {
        return Err(LimitViolation::ResultsAboveBucket);
    }
    Ok(())
}

/// A violated Kademlia cross-field rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LimitViolation {
    /// `target_routing_peers > max_routing_peers`.
    TargetAboveMax,
    /// `bootstrap_refresh_interval < bootstrap_min_interval`.
    RefreshBelowMinimum,
    /// `max_results_per_query > kbucket_size`.
    ResultsAboveBucket,
}

impl core::fmt::Display for LimitViolation {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let s = match self {
            Self::TargetAboveMax => "target_routing_peers exceeds max_routing_peers",
            Self::RefreshBelowMinimum => {
                "bootstrap_refresh_interval is below bootstrap_min_interval"
            }
            Self::ResultsAboveBucket => "max_results_per_query exceeds kbucket_size",
        };
        f.write_str(s)
    }
}

impl core::error::Error for LimitViolation {}

#[cfg(test)]
mod tests {

    #[test]
    fn the_bound_is_a_property_of_the_value_and_not_of_one_path_to_it() {
        // THE FIRST FIX WAS ON THE WRONG PATH, and its own test said so:
        // it noted that this port is crossed as a Rust value in-process
        // and that serde exists here for fixtures and diagnostics. Both
        // true, and together they say a `deserialize_with` ceiling
        // guards the path nobody uses. A provider writing
        // `OfferRoutingPeer { addresses: vec![..] }` -- the runtime path
        // the port is designed for -- went straight past it.
        let peer = "12D3KooWDpJ7As7BWAwRMfu1VU2WCqNjvq387JEYKDBj4kx6nXTN";
        let addr = |i: usize| format!("/ip4/10.0.0.1/tcp/{i}");

        // The Rust path, which is the one that matters.
        let strs: Vec<String> = (0..MAX_ADDRESSES).map(addr).collect();
        assert!(OfferedAddresses::parse_all(strs.iter().map(String::as_str)).is_ok());
        let over_strs: Vec<String> = (0..=MAX_ADDRESSES).map(addr).collect();
        assert_eq!(
            OfferedAddresses::parse_all(over_strs.iter().map(String::as_str)),
            Err(PortLimit::TooManyAddresses {
                got: MAX_ADDRESSES + 1
            })
        );

        // THE LENGTH IS JUDGED ON THE BORROWED SLICE, before a copy is
        // made. Checking it after the item is yielded is too late: by
        // then `next()` has finished building the `String`, so a single
        // oversized item is attacker-controlled memory already spent.
        // `parse` is where a caller can still act, because it is the
        // last point at which the value is not yet owned.
        let long = "a".repeat(MAX_ADDRESS_BYTES + 1);
        assert_eq!(
            OfferedAddress::parse(&long),
            Err(PortLimit::AddressOutOfBounds {
                got: MAX_ADDRESS_BYTES + 1
            })
        );
        assert_eq!(
            OfferedAddress::parse(""),
            Err(PortLimit::AddressOutOfBounds { got: 0 })
        );
        assert!(OfferedAddress::parse(&"a".repeat(MAX_ADDRESS_BYTES)).is_ok());

        // And the collection cannot be built around it: there is no
        // constructor taking a raw `String`, so an unchecked address
        // has no route in.
        assert_eq!(
            OfferedAddresses::parse_all([long.as_str()]),
            Err(PortLimit::AddressOutOfBounds {
                got: MAX_ADDRESS_BYTES + 1
            })
        );

        // There is no second door. `KademliaCommand::OfferRoutingPeer`
        // cannot be built with a raw `Vec`, which is the whole point of
        // the newtype: this compiles only because the value went
        // through the constructor.
        let cmd = KademliaCommand::OfferRoutingPeer {
            peer: TransportIdentity::parse(peer).expect("canonical"),
            addresses: OfferedAddresses::parse_all([addr(1).as_str()]).expect("one address"),
        };
        match &cmd {
            KademliaCommand::OfferRoutingPeer { addresses, .. } => {
                assert_eq!(addresses.len(), 1);
            }
            _ => panic!("wrong variant"),
        }

        // AND IT STOPS READING. `collect()` then check is the same
        // defect this crate fixed on the wire path and reintroduced in
        // the constructor: it materializes the whole input before the
        // bound can look. Proved by an iterator that PANICS one item
        // past the point the check should have stopped at -- reaching
        // it means the constructor read further than it needed to.
        let over = (0..).map(|i| {
            assert!(i <= MAX_ADDRESSES, "read past the limit at item {i}");
            OfferedAddress::parse(&addr(i)).expect("legal")
        });
        assert_eq!(
            OfferedAddresses::new(over),
            Err(PortLimit::TooManyAddresses {
                got: MAX_ADDRESSES + 1
            })
        );

        // Results, same shape.
        let candidate = || {
            serde_json::from_str::<CandidatePeer>(&format!(
                r#"{{"peer_id":"{peer}","addresses":["/ip4/10.0.0.1/tcp/1"],"source":"kademlia","observed_at":1}}"#
            ))
            .expect("a valid candidate")
        };
        assert!(ObservedCandidates::new((0..MAX_RESULTS_PER_QUERY).map(|_| candidate())).is_ok());
        assert_eq!(
            ObservedCandidates::new((0..=MAX_RESULTS_PER_QUERY).map(|_| candidate())),
            Err(PortLimit::TooManyResults {
                got: MAX_RESULTS_PER_QUERY + 1
            })
        );

        let over = (0..).map(|i| {
            assert!(
                i <= MAX_RESULTS_PER_QUERY,
                "read past the limit at item {i}"
            );
            candidate()
        });
        assert_eq!(
            ObservedCandidates::new(over),
            Err(PortLimit::TooManyResults {
                got: MAX_RESULTS_PER_QUERY + 1
            })
        );

        // And the wire path still refuses, arriving at the SAME
        // constructor rather than beside it.
        let addrs = |n: usize| {
            (0..n)
                .map(|i| format!(r#""{}""#, addr(i)))
                .collect::<Vec<_>>()
                .join(",")
        };
        let offer = |body: String| {
            serde_json::from_str::<KademliaCommand>(&format!(
                r#"{{"command":"offer_routing_peer","peer":"{peer}","addresses":[{body}]}}"#
            ))
        };
        assert!(offer(addrs(MAX_ADDRESSES)).is_ok(), "the ceiling is legal");
        assert!(offer(addrs(MAX_ADDRESSES + 1)).is_err());
        assert!(offer(r#""""#.to_owned()).is_err(), "an empty address");
        let long = "a".repeat(MAX_ADDRESS_BYTES + 1);
        assert!(offer(format!(r#""{long}""#)).is_err());
    }
    use super::*;
    use interweave_discovery_api::ProviderHealth;

    fn view(routing: u32, target: u32, max: u32, trusted: u32, stalled: u32) -> RoutingView {
        RoutingView {
            routing_peers: routing,
            target_routing_peers: target,
            max_routing_peers: max,
            remote_trusted_population: trusted,
            no_progress_rounds: stalled,
        }
    }

    #[test]
    fn the_effective_target_is_capped_by_the_trusted_population() {
        // The term that matters: a profile trusting two peers cannot reach
        // a target of 64 however long it explores.
        assert_eq!(view(0, 64, 256, 2, 0).effective_target(), 2);
        assert_eq!(view(0, 64, 256, 999, 0).effective_target(), 64);
        assert_eq!(view(0, 64, 32, 999, 0).effective_target(), 32);
        assert_eq!(view(0, 64, 256, 0, 0).effective_target(), 0);
    }

    #[test]
    fn a_small_trusted_overlay_reaches_its_target() {
        let v = view(2, 64, 256, 2, 0);
        assert!(v.is_target_satisfied());
        assert_eq!(v.health(true), ProviderHealth::Healthy);
    }

    #[test]
    fn saturation_is_a_resting_state_not_a_failure() {
        // Below target and still healthy: exploration ran out of peers to
        // find, which is not the same as being broken.
        let v = view(1, 64, 256, 8, SATURATION_ROUNDS);
        assert!(!v.is_target_satisfied());
        assert!(v.is_saturated());
        assert_eq!(v.health(true), ProviderHealth::Healthy);

        // One round short is still merely degraded.
        let v = view(1, 64, 256, 8, SATURATION_ROUNDS - 1);
        assert!(!v.is_saturated());
        assert_eq!(v.health(true), ProviderHealth::Degraded);
    }

    #[test]
    fn no_trusted_peers_means_unavailable_not_degraded() {
        let v = view(0, 64, 256, 0, 99);
        assert!(!v.can_become_healthy());
        assert_eq!(v.health(true), ProviderHealth::Unavailable);
    }

    #[test]
    fn failing_queries_degrade_even_a_satisfied_view() {
        let v = view(64, 64, 256, 999, 0);
        assert!(v.is_target_satisfied());
        assert_eq!(v.health(false), ProviderHealth::Degraded);
    }

    #[test]
    fn exploration_backs_off_and_is_capped() {
        let base = 60_000;
        assert_eq!(
            view(0, 8, 32, 8, 0).next_exploration_interval_ms(base),
            60_000
        );
        assert_eq!(
            view(0, 8, 32, 8, 1).next_exploration_interval_ms(base),
            120_000
        );
        assert_eq!(
            view(0, 8, 32, 8, 2).next_exploration_interval_ms(base),
            240_000
        );
        // Capped rather than growing without bound.
        assert_eq!(
            view(0, 8, 32, 8, 99).next_exploration_interval_ms(base),
            MAX_EXPLORATION_INTERVAL_MS
        );
        assert!(
            view(0, 8, 32, 8, 60).next_exploration_interval_ms(base) <= MAX_EXPLORATION_INTERVAL_MS
        );
    }

    #[test]
    fn cross_field_limits_are_checkable() {
        assert_eq!(validate_limits(64, 256, 300_000, 900_000, 20, 20), Ok(()));
        assert_eq!(
            validate_limits(512, 256, 300_000, 900_000, 20, 20),
            Err(LimitViolation::TargetAboveMax)
        );
        assert_eq!(
            validate_limits(64, 256, 900_000, 300_000, 20, 20),
            Err(LimitViolation::RefreshBelowMinimum)
        );
        assert_eq!(
            validate_limits(64, 256, 300_000, 900_000, 20, 8),
            Err(LimitViolation::ResultsAboveBucket)
        );
    }

    #[test]
    fn the_command_port_has_no_record_or_dial_variant() {
        // The rule is structural, so the check is too: every variant is
        // matched exhaustively, and adding a record or dial command would
        // fail to compile here before a reviewer ever saw it.
        let commands = [
            KademliaCommand::SetMode {
                mode: KademliaMode::Client,
            },
            KademliaCommand::OfferRoutingPeer {
                peer: TransportIdentity::parse(
                    "12D3KooWDpJ7As7BWAwRMfu1VU2WCqNjvq387JEYKDBj4kx6nXTN",
                )
                .expect("valid"),
                addresses: OfferedAddresses::new([]).expect("empty is legal"),
            },
            KademliaCommand::StartQuery {
                class: QueryClass::Exploration,
                key: [0u8; 32],
            },
            KademliaCommand::Shutdown,
        ];
        for c in &commands {
            match c {
                KademliaCommand::SetMode { .. }
                | KademliaCommand::OfferRoutingPeer { .. }
                | KademliaCommand::StartQuery { .. }
                | KademliaCommand::Shutdown => {}
            }
        }
        assert_eq!(commands.len(), 4, "the port gained a command");
    }

    #[test]
    fn commands_and_events_serialize_with_their_discriminants() {
        let c = KademliaCommand::StartQuery {
            class: QueryClass::Bootstrap,
            key: [1u8; 32],
        };
        let json = serde_json::to_value(&c).expect("ser");
        assert_eq!(json["command"], "start_query");
        assert_eq!(json["class"], "bootstrap");
        assert_eq!(
            serde_json::from_value::<KademliaCommand>(json).expect("de"),
            c
        );

        let e = KademliaEvent::QueryFailed {
            class: QueryClass::Targeted,
            reason: QueryFailure::TimedOut,
        };
        let json = serde_json::to_value(&e).expect("ser");
        assert_eq!(json["event"], "query_failed");
        assert_eq!(json["reason"], "timed_out");
    }

    #[test]
    fn a_query_key_is_exactly_thirty_two_bytes() {
        // A fixed array, so a key cannot be the wrong width and cannot
        // smuggle a payload: this is peer routing, and the key space is
        // the identifier space.
        let short: Vec<u8> = vec![0; 31];
        let json = serde_json::json!({
            "command": "start_query",
            "class": "exploration",
            "key": short
        });
        assert!(serde_json::from_value::<KademliaCommand>(json).is_err());
    }
}
