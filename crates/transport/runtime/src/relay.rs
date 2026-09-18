// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! Relay reservation policy (Circuit Relay v2 client, `RELAY.md` §§3-5,
//! `transport/libp2p/CONNECTIVITY.md` §8).
//!
//! The libp2p relay client obtains a reservation by LISTENING on
//! `<relay>/p2p-circuit`, renews it on its own, and reports its loss as
//! the listener closing. What it does not decide -- and what this
//! manager does -- is WHICH relays, HOW MANY, WHEN to ask again after a
//! failure, and which relay-derived addresses this profile advertises
//! at any moment. It opens no socket and reads no clock: every method
//! that can expire anything takes `now_ms`, so the transitions are
//! testable by enumeration, the way [`crate::reachability`] is.
//!
//! # The rules it carries
//!
//! - **Targets follow the direct-inbound verdict** (§8's table): two
//!   active reservations while direct inbound is `Unknown` or
//!   `NotVerified`, one while `VerifiedPublic`, never more than the
//!   configured maximum, and never more than the eligible candidates
//!   can supply -- a deployment with one relay is [`Standing::Partial`],
//!   not an acquisition storm.
//! - **Static relays precede learned ones** (`RELAY.md` §3): a learned
//!   relay is asked only when the static ones cannot fill the target,
//!   and whether learned relays exist at all is the adapter's opt-in.
//!   A surplus -- the target fell -- releases learned reservations
//!   first, newest first.
//! - **A failed relay backs off on its own** (`RELAY.md` §4): the retry
//!   delay doubles from the configured minimum to the configured
//!   maximum per relay, with the jitter the adapter supplies, and a
//!   success resets only that relay's ladder. Nothing here backs off
//!   the DIAL of the relay's control connection: that is the outbound
//!   gate's, under `DialOrigin::RelayReservation`.
//! - **An address is advertised only while its reservation is active**
//!   (`RELAY.md` §5): [`ReservationManager::advertised`] is the set, and
//!   a loss, a release or a de-authorisation removes the address in the
//!   same call that records it.
//!
//! # What it refuses, and counts
//!
//! A report about a relay never offered, an acceptance for a relay
//! nothing asked, or a failure for a relay that is idle, is refused by
//! name ([`RefusedRelayReport`]) rather than folded in as a fresh
//! reservation or a fresh backoff: an address this profile never chose
//! must not be advertised on the say-so of the peer that sent it, and
//! the close of a listener this manager gave up is not the relay's
//! fault. The adapter counts each refusal under `RELAY.md` §11's
//! `relay_reservation_events_total{outcome}`.

use std::collections::BTreeMap;

use interweave_transport_api::{DirectInboundState, TransportIdentity};

/// Static relays a profile may configure (`config.schema.yaml`:
/// `static_relays: list[.., max=16]`).
pub const MAX_STATIC_RELAYS: usize = 16;

/// Identify-learned relays kept beside the static ones. The same
/// figure, for the same reason the AutoNAT adapter keeps sixteen: an
/// authorized peer advertising the hop protocol is a candidate, and the
/// authorized set is bounded already; this bounds the subset kept here.
pub const MAX_LEARNED_RELAYS: usize = 16;

/// Addresses kept per relay. The list is written by whoever offered the
/// relay -- the operator for a static one, the peer itself through
/// Identify for a learned one, and a peer pushes Identify as often as it
/// likes -- so it is bounded the way the connection manager bounds the
/// same input, and the bound is the same figure.
pub const MAX_ADDRESSES_PER_RELAY: usize = 8;

/// The largest `max_reservations` the profile allows (`integer[1..8]`).
pub const MAX_RESERVATIONS_CEILING: u32 = 8;

/// `RELAY.md` §4's defaults: 5 s minimum, 5 min maximum.
pub const DEFAULT_RETRY_MIN_MS: u64 = 5_000;
/// See [`DEFAULT_RETRY_MIN_MS`].
pub const DEFAULT_RETRY_MAX_MS: u64 = 5 * 60_000;

/// The reservation targets and the retry ladder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReservationConfig {
    /// Active reservations wanted while direct inbound is `Unknown` or
    /// `NotVerified`.
    pub target_private_or_unknown: u32,
    /// Active reservations wanted while direct inbound is
    /// `VerifiedPublic`. May be zero.
    pub target_public: u32,
    /// Reservations held at once, whatever the target says.
    pub max_reservations: u32,
    /// The first retry delay after a failure.
    pub retry_min_ms: u64,
    /// The delay the ladder stops doubling at.
    pub retry_max_ms: u64,
}

impl Default for ReservationConfig {
    /// `CONNECTIVITY.md` §8: 2, 1, 4; 5 s to 5 min.
    fn default() -> Self {
        Self {
            target_private_or_unknown: 2,
            target_public: 1,
            max_reservations: 4,
            retry_min_ms: DEFAULT_RETRY_MIN_MS,
            retry_max_ms: DEFAULT_RETRY_MAX_MS,
        }
    }
}

/// Why a configuration is refused at construction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigError {
    /// `max_reservations` is zero or above [`MAX_RESERVATIONS_CEILING`].
    MaxReservations,
    /// A target exceeds `max_reservations`, or the public target exceeds
    /// the private one.
    Targets,
    /// `retry_min_ms` is zero or exceeds `retry_max_ms`.
    Retry,
}

/// Where a candidate came from; static relays are asked first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum RelaySource {
    /// Configured by the operator.
    Static,
    /// Advertised through Identify by an authorized peer, kept only
    /// when the adapter's opt-in says so.
    Learned,
}

/// One relay's place in the state machine of `RELAY.md` §4.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReservationState {
    /// Not asked.
    Idle,
    /// Asked; the adapter is dialling and reserving.
    Requested {
        /// When it was asked.
        since_ms: u64,
        /// Failures in a row before this ask, carried so a failure of
        /// the ask continues the ladder rather than restarting it;
        /// only an acceptance resets it.
        attempts: u32,
    },
    /// Held: the relay accepted, and `address` is advertised.
    Active {
        /// When it was accepted; a renewal keeps it.
        since_ms: u64,
        /// The relay-derived address the crate produced.
        address: String,
    },
    /// Failed or lost; not asked again before `until_ms`.
    Backoff {
        /// When the relay may be asked again.
        until_ms: u64,
        /// Failures in a row, which set the next delay.
        attempts: u32,
    },
}

/// A relay this manager knows about.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Candidate {
    /// The addresses to reach it, as configured or learned; at most
    /// [`MAX_ADDRESSES_PER_RELAY`].
    addresses: Vec<String>,
    source: RelaySource,
    state: ReservationState,
}

impl Candidate {
    /// Add an address; `true` when it is now in the list, `false` when
    /// the list is full and it is not.
    fn add_address(&mut self, address: &str) -> bool {
        if self.addresses.iter().any(|a| a == address) {
            return true;
        }
        if self.addresses.len() >= MAX_ADDRESSES_PER_RELAY {
            return false;
        }
        self.addresses.push(address.to_owned());
        true
    }
}

/// What the adapter should do after a tick.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Ask this relay for a reservation, reaching it at these addresses.
    Reserve {
        /// The relay.
        relay: TransportIdentity,
        /// Its addresses, static ones as configured.
        addresses: Vec<String>,
    },
    /// Give this reservation up: the target fell below what is held.
    /// The address it carried is already gone from
    /// [`ReservationManager::advertised`].
    Release {
        /// The relay.
        relay: TransportIdentity,
        /// The address that was advertised for it.
        address: String,
    },
}

/// A report the manager refused, by name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefusedRelayReport {
    /// The relay was never offered: an acceptance from a stranger must
    /// not put an address this profile never chose into the advertised
    /// set.
    UnknownRelay,
    /// The relay was offered but never asked: an acceptance nothing
    /// requested is not a reservation this manager holds.
    UnrequestedAcceptance,
    /// The relay is idle -- never asked, or released by this manager --
    /// so a failure reported for it is the echo of a release, not a
    /// fault to back off from.
    UnrequestedFailure,
    /// An acceptance carrying no address: nothing to advertise.
    EmptyAddress,
}

impl std::fmt::Display for RefusedRelayReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::UnknownRelay => "unknown relay",
            Self::UnrequestedAcceptance => "unrequested acceptance",
            Self::UnrequestedFailure => "unrequested failure",
            Self::EmptyAddress => "empty address",
        })
    }
}

/// Whether the target is met (`CONNECTIVITY.md` §8: "can be `Partial`,
/// not stuck in an infinite acquisition storm").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Standing {
    /// As many active reservations as the target asks.
    Satisfied,
    /// Fewer. Whether the next tick can ask for more is
    /// [`ReservationManager::askable_now`]; when that is zero every
    /// other candidate is requested, backing off, or does not exist,
    /// and the shortfall is the deployment's, not a storm.
    Partial,
    /// The target is zero: configured so for this verdict, or there
    /// are no candidates -- [`ReservationManager::candidates`] says
    /// which.
    None,
}

/// The policy half of the relay client.
#[derive(Debug, Clone)]
pub struct ReservationManager {
    config: ReservationConfig,
    candidates: BTreeMap<TransportIdentity, Candidate>,
    direct_inbound: DirectInboundState,
}

impl ReservationManager {
    /// Build a manager under `config`.
    ///
    /// # Errors
    /// [`ConfigError`] for a zero or over-ceiling maximum, targets that
    /// exceed it or each other the wrong way round, or a retry ladder
    /// that is zero or inverted -- a subset of the rules `profile-config`
    /// checks (its floors are higher), refused again here because the
    /// manager is the bound.
    pub fn new(config: ReservationConfig) -> Result<Self, ConfigError> {
        if config.max_reservations == 0 || config.max_reservations > MAX_RESERVATIONS_CEILING {
            return Err(ConfigError::MaxReservations);
        }
        if config.target_private_or_unknown > config.max_reservations
            || config.target_public > config.max_reservations
            || config.target_public > config.target_private_or_unknown
        {
            return Err(ConfigError::Targets);
        }
        if config.retry_min_ms == 0 || config.retry_min_ms > config.retry_max_ms {
            return Err(ConfigError::Retry);
        }
        Ok(Self {
            config,
            candidates: BTreeMap::new(),
            direct_inbound: DirectInboundState::Unknown,
        })
    }

    /// The configuration in force.
    #[must_use]
    pub const fn config(&self) -> &ReservationConfig {
        &self.config
    }

    /// Offer a configured relay. A second address for a known static
    /// relay is added to its list; a relay already learned is promoted
    /// to static, keeping its state. `false` when the static set is
    /// full -- a promotion counts against it too -- or the relay holds
    /// [`MAX_ADDRESSES_PER_RELAY`] addresses already, or the address is
    /// empty.
    #[must_use = "a refused relay is a configured relay the adapter never asks"]
    pub fn add_static(&mut self, relay: TransportIdentity, address: &str) -> bool {
        if address.is_empty() {
            return false;
        }
        let static_full = self.count(RelaySource::Static) >= MAX_STATIC_RELAYS;
        if let Some(candidate) = self.candidates.get_mut(&relay) {
            if candidate.source == RelaySource::Learned && static_full {
                return false;
            }
            candidate.source = RelaySource::Static;
            return candidate.add_address(address);
        }
        if static_full {
            return false;
        }
        self.candidates.insert(
            relay,
            Candidate {
                addresses: vec![address.to_owned()],
                source: RelaySource::Static,
                state: ReservationState::Idle,
            },
        );
        true
    }

    /// Offer a relay Identify advertised. The adapter calls this only
    /// under the operator's opt-in and only for an authorized peer; the
    /// manager keeps at most [`MAX_LEARNED_RELAYS`], each with at most
    /// [`MAX_ADDRESSES_PER_RELAY`] addresses. A static relay is left as
    /// it is: its addresses are the operator's, and a peer-asserted one
    /// is not added to them (`true`, since the relay is known). `false`
    /// when refused for room or an empty address.
    #[must_use = "a refused relay is a candidate the adapter believes it offered"]
    pub fn learn(&mut self, relay: TransportIdentity, address: &str) -> bool {
        if address.is_empty() {
            return false;
        }
        if let Some(candidate) = self.candidates.get_mut(&relay) {
            return match candidate.source {
                RelaySource::Static => true,
                RelaySource::Learned => candidate.add_address(address),
            };
        }
        if self.count(RelaySource::Learned) >= MAX_LEARNED_RELAYS {
            return false;
        }
        self.candidates.insert(
            relay,
            Candidate {
                addresses: vec![address.to_owned()],
                source: RelaySource::Learned,
                state: ReservationState::Idle,
            },
        );
        true
    }

    /// Drop a relay -- it lost its authorization, or a learned one is
    /// no longer wanted. Returns the address that was advertised for it,
    /// if its reservation was active, so the adapter withdraws it.
    #[must_use = "the address is already gone from advertised(); the adapter withdraws it"]
    pub fn forget(&mut self, relay: &TransportIdentity) -> Option<String> {
        let candidate = self.candidates.remove(relay)?;
        match candidate.state {
            ReservationState::Active { address, .. } => Some(address),
            _ => None,
        }
    }

    /// The direct-inbound verdict, which sets the target. Returns the
    /// reservations to release when the target fell below what is
    /// held: learned relays first, then static, newest first, so a
    /// profile that became reachable keeps its oldest configured relay
    /// warm. Their addresses are gone from [`Self::advertised`] on
    /// return. A tick should follow, as after every report: an
    /// acceptance that arrives after the target fell is advertised
    /// until the next tick releases it.
    #[must_use = "the Release actions' addresses are already gone from advertised()"]
    pub fn set_direct_inbound(&mut self, state: DirectInboundState) -> Vec<Action> {
        self.direct_inbound = state;
        self.release_surplus()
    }

    /// The direct-inbound verdict the target follows.
    #[must_use]
    pub const fn direct_inbound(&self) -> DirectInboundState {
        self.direct_inbound
    }

    /// How many active reservations are wanted right now: the verdict's
    /// target, capped at `max_reservations` and at the candidates that
    /// exist.
    #[must_use]
    pub fn target(&self) -> usize {
        let wanted = match self.direct_inbound {
            DirectInboundState::VerifiedPublic => self.config.target_public,
            DirectInboundState::Unknown | DirectInboundState::NotVerified => {
                self.config.target_private_or_unknown
            }
        };
        let cap = self.config.max_reservations.min(wanted) as usize;
        cap.min(self.candidates.len())
    }

    /// Relays known, static and learned.
    #[must_use]
    pub fn candidates(&self) -> usize {
        self.candidates.len()
    }

    /// Reservations held.
    #[must_use]
    pub fn active(&self) -> usize {
        self.candidates
            .values()
            .filter(|c| matches!(c.state, ReservationState::Active { .. }))
            .count()
    }

    /// Reservations asked for and not yet answered.
    #[must_use]
    pub fn requested(&self) -> usize {
        self.candidates
            .values()
            .filter(|c| matches!(c.state, ReservationState::Requested { .. }))
            .count()
    }

    /// The relay-derived addresses this profile advertises: one per
    /// active reservation, and nothing else.
    #[must_use]
    pub fn advertised(&self) -> Vec<String> {
        self.candidates
            .values()
            .filter_map(|c| match &c.state {
                ReservationState::Active { address, .. } => Some(address.clone()),
                _ => None,
            })
            .collect()
    }

    /// Where the target stands.
    #[must_use]
    pub fn standing(&self) -> Standing {
        let target = self.target();
        if target == 0 {
            Standing::None
        } else if self.active() >= target {
            Standing::Satisfied
        } else {
            Standing::Partial
        }
    }

    /// Relays the next tick could ask: idle, or backed off and due.
    #[must_use]
    pub fn askable_now(&self, now_ms: u64) -> usize {
        self.askable(now_ms).count()
    }

    /// One relay's state.
    #[must_use]
    pub fn state(&self, relay: &TransportIdentity) -> Option<&ReservationState> {
        self.candidates.get(relay).map(|c| &c.state)
    }

    /// One relay's source.
    #[must_use]
    pub fn source(&self, relay: &TransportIdentity) -> Option<RelaySource> {
        self.candidates.get(relay).map(|c| c.source)
    }

    /// Decide what to ask for now.
    ///
    /// Asks, in order -- static before learned, then by identity so the
    /// order is stable -- as many idle or due relays as it takes to
    /// bring active-plus-requested up to the target; each asked relay
    /// moves to `Requested`. Then releases any surplus (the target may
    /// have fallen since the last tick).
    #[must_use = "the actions are the reservations the adapter must ask for or give up"]
    pub fn tick(&mut self, now_ms: u64) -> Vec<Action> {
        let mut actions = Vec::new();
        let target = self.target();
        let holding = self.active() + self.requested();
        let to_ask: Vec<TransportIdentity> = self
            .askable(now_ms)
            .take(target.saturating_sub(holding))
            .collect();
        for relay in to_ask {
            if let Some(candidate) = self.candidates.get_mut(&relay) {
                let attempts = match candidate.state {
                    ReservationState::Backoff { attempts, .. } => attempts,
                    _ => 0,
                };
                candidate.state = ReservationState::Requested {
                    since_ms: now_ms,
                    attempts,
                };
                actions.push(Action::Reserve {
                    relay,
                    addresses: candidate.addresses.clone(),
                });
            }
        }
        actions.extend(self.release_surplus());
        actions
    }

    /// Record the relay's acceptance -- the crate's listener producing
    /// `address` -- or its renewal. A renewal keeps the acceptance time,
    /// so a surplus release still counts age from the acceptance; a
    /// renewal that produced a different address supersedes the one
    /// advertised, which is returned so the adapter withdraws it, gone
    /// from [`Self::advertised`] on return.
    ///
    /// # Errors
    /// [`RefusedRelayReport`] for a relay never offered, one that was
    /// offered but not asked (idle, or backing off), or an empty
    /// address: nothing is advertised in any of the three.
    pub fn record_accepted(
        &mut self,
        relay: &TransportIdentity,
        address: &str,
        now_ms: u64,
    ) -> Result<Option<String>, RefusedRelayReport> {
        if address.is_empty() {
            return Err(RefusedRelayReport::EmptyAddress);
        }
        let candidate = self
            .candidates
            .get_mut(relay)
            .ok_or(RefusedRelayReport::UnknownRelay)?;
        let (since_ms, superseded) = match &candidate.state {
            ReservationState::Requested { .. } => (now_ms, None),
            ReservationState::Active {
                since_ms,
                address: held,
            } => (*since_ms, (held != address).then(|| held.clone())),
            ReservationState::Idle | ReservationState::Backoff { .. } => {
                return Err(RefusedRelayReport::UnrequestedAcceptance);
            }
        };
        candidate.state = ReservationState::Active {
            since_ms,
            address: address.to_owned(),
        };
        Ok(superseded)
    }

    /// Record that the relay refused, the reservation was lost, or the
    /// attempt failed. The relay backs off on its own ladder --
    /// `retry_min_ms` doubling to `retry_max_ms`, plus `jitter_ms`,
    /// which the adapter draws and which is capped at the delay itself
    /// -- and its address, if one was advertised, is returned so the
    /// adapter withdraws it.
    ///
    /// # Errors
    /// [`RefusedRelayReport::UnknownRelay`] for a relay never offered;
    /// [`RefusedRelayReport::UnrequestedFailure`] for one that is idle
    /// -- the close of a listener this manager released, reported back
    /// as a loss, is not a failure of the relay and does not back it
    /// off.
    pub fn record_failed(
        &mut self,
        relay: &TransportIdentity,
        now_ms: u64,
        jitter_ms: u64,
    ) -> Result<Option<String>, RefusedRelayReport> {
        let candidate = self
            .candidates
            .get_mut(relay)
            .ok_or(RefusedRelayReport::UnknownRelay)?;
        let (withdrawn, attempts) = match &candidate.state {
            ReservationState::Active { address, .. } => (Some(address.clone()), 0),
            ReservationState::Backoff { attempts, .. }
            | ReservationState::Requested { attempts, .. } => (None, *attempts),
            ReservationState::Idle => return Err(RefusedRelayReport::UnrequestedFailure),
        };
        let delay = self.config.retry_delay_ms(attempts);
        candidate.state = ReservationState::Backoff {
            until_ms: now_ms
                .saturating_add(delay)
                .saturating_add(jitter_ms.min(delay)),
            attempts: attempts.saturating_add(1),
        };
        Ok(withdrawn)
    }

    /// Relays that may be asked now, static before learned, then by
    /// identity.
    fn askable(&self, now_ms: u64) -> impl Iterator<Item = TransportIdentity> + '_ {
        let mut order: Vec<(&TransportIdentity, &Candidate)> = self
            .candidates
            .iter()
            .filter(|(_, c)| match &c.state {
                ReservationState::Idle => true,
                ReservationState::Backoff { until_ms, .. } => *until_ms <= now_ms,
                ReservationState::Requested { .. } | ReservationState::Active { .. } => false,
            })
            .collect();
        order.sort_by(|(ia, ca), (ib, cb)| ca.source.cmp(&cb.source).then_with(|| ia.cmp(ib)));
        order.into_iter().map(|(id, _)| id.clone())
    }

    /// Release active reservations above the target: learned first,
    /// then static, newest first.
    fn release_surplus(&mut self) -> Vec<Action> {
        let target = self.target();
        let active = self.active();
        if active <= target {
            return Vec::new();
        }
        let mut held: Vec<(TransportIdentity, RelaySource, u64)> = self
            .candidates
            .iter()
            .filter_map(|(id, c)| match &c.state {
                ReservationState::Active { since_ms, .. } => {
                    Some((id.clone(), c.source, *since_ms))
                }
                _ => None,
            })
            .collect();
        // Learned before static (Learned > Static in the enum's order),
        // then the newest first.
        held.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| b.2.cmp(&a.2)));
        let mut actions = Vec::new();
        for (relay, _, _) in held.into_iter().take(active - target) {
            if let Some(candidate) = self.candidates.get_mut(&relay)
                && let ReservationState::Active { address, .. } = &candidate.state
            {
                let address = address.clone();
                candidate.state = ReservationState::Idle;
                actions.push(Action::Release { relay, address });
            }
        }
        actions
    }

    fn count(&self, source: RelaySource) -> usize {
        self.candidates
            .values()
            .filter(|c| c.source == source)
            .count()
    }
}

impl ReservationConfig {
    /// The retry delay after `attempts` failures in a row: the minimum
    /// doubled `attempts` times, stopping at the maximum.
    #[must_use]
    pub fn retry_delay_ms(&self, attempts: u32) -> u64 {
        let doubled = self.retry_min_ms.saturating_mul(1u64 << attempts.min(20));
        doubled.min(self.retry_max_ms)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::panic)]

    use super::*;

    const R1: &str = "12D3KooWDpJ7As7BWAwRMfu1VU2WCqNjvq387JEYKDBj4kx6nXTN";
    const R2: &str = "12D3KooWHyNGMf9HTd3Zj6dStdkcc5ycsubW1rEgQSp6k6yfZBoy";
    const R3: &str = "12D3KooWQYhTNQdmr3ArTeUHRYzFg94BKyTkoWBDWez9kSCVe2Xo";

    fn ident(s: &str) -> TransportIdentity {
        TransportIdentity::parse(s).expect("a canonical identity")
    }

    /// A distinct canonical identity for bound tests: the base58 tail
    /// is rewritten from `i`, and the neutral grammar decodes the
    /// result, so each is a real PeerId rather than a spelled string.
    fn nth(i: usize) -> TransportIdentity {
        let mut bytes = bs58::decode(R1).into_vec().expect("base58");
        let len = bytes.len();
        bytes[len - 1] = (i % 251) as u8;
        bytes[len - 2] = (i / 251) as u8;
        TransportIdentity::parse(bs58::encode(bytes).into_string()).expect("canonical")
    }

    fn manager() -> ReservationManager {
        ReservationManager::new(ReservationConfig::default()).expect("valid")
    }

    fn with_static(relays: &[&str]) -> ReservationManager {
        let mut m = manager();
        for (i, r) in relays.iter().enumerate() {
            assert!(m.add_static(ident(r), &format!("/ip4/192.0.2.{}/tcp/4001", i + 1)));
        }
        m
    }

    fn reserves(actions: &[Action]) -> Vec<TransportIdentity> {
        actions
            .iter()
            .filter_map(|a| match a {
                Action::Reserve { relay, .. } => Some(relay.clone()),
                Action::Release { .. } => None,
            })
            .collect()
    }

    #[test]
    fn a_configuration_the_profile_would_refuse_is_refused_here_too() {
        let ok = ReservationConfig::default();
        assert!(ReservationManager::new(ok.clone()).is_ok());
        let rows: [(ReservationConfig, ConfigError); 6] = [
            (
                ReservationConfig {
                    max_reservations: 0,
                    ..ok.clone()
                },
                ConfigError::MaxReservations,
            ),
            (
                ReservationConfig {
                    max_reservations: MAX_RESERVATIONS_CEILING + 1,
                    ..ok.clone()
                },
                ConfigError::MaxReservations,
            ),
            (
                ReservationConfig {
                    target_private_or_unknown: 5,
                    ..ok.clone()
                },
                ConfigError::Targets,
            ),
            (
                ReservationConfig {
                    target_public: 3,
                    target_private_or_unknown: 2,
                    ..ok.clone()
                },
                ConfigError::Targets,
            ),
            (
                ReservationConfig {
                    retry_min_ms: 0,
                    ..ok.clone()
                },
                ConfigError::Retry,
            ),
            (
                ReservationConfig {
                    retry_min_ms: 10,
                    retry_max_ms: 5,
                    ..ok.clone()
                },
                ConfigError::Retry,
            ),
        ];
        for (config, expected) in rows {
            assert_eq!(ReservationManager::new(config).err(), Some(expected));
        }
    }

    #[test]
    fn the_target_follows_the_verdict_and_is_capped_by_the_maximum_and_the_population() {
        // CONNECTIVITY.md section 8's table: 2 while unknown or not
        // verified, 1 when public, never more than max, never more than
        // the relays that exist.
        let mut m = with_static(&[R1, R2, R3]);
        assert_eq!(m.direct_inbound(), DirectInboundState::Unknown);
        assert_eq!(m.target(), 2);
        let _ = m.set_direct_inbound(DirectInboundState::NotVerified);
        assert_eq!(m.target(), 2);
        let _ = m.set_direct_inbound(DirectInboundState::VerifiedPublic);
        assert_eq!(m.target(), 1);
        // One relay: the target is the population, and the standing is
        // Partial rather than a storm.
        let mut one = with_static(&[R1]);
        assert_eq!(one.target(), 1);
        let asked = one.tick(0);
        assert_eq!(reserves(&asked), vec![ident(R1)]);
        assert_eq!(one.standing(), Standing::Partial);
        assert_eq!(one.askable_now(0), 0, "and nothing else can be asked");
        assert!(one.tick(1).is_empty(), "so the next tick asks nothing");
        // A public target of zero is a coherent posture.
        let mut none = ReservationManager::new(ReservationConfig {
            target_public: 0,
            ..ReservationConfig::default()
        })
        .expect("valid");
        assert!(none.add_static(ident(R1), "/ip4/192.0.2.1/tcp/1"));
        let _ = none.set_direct_inbound(DirectInboundState::VerifiedPublic);
        assert_eq!(none.target(), 0);
        assert_eq!(none.standing(), Standing::None);
        assert!(none.tick(0).is_empty());
        // ... and so is having nobody to ask; candidates() tells the two
        // apart.
        assert_eq!(none.candidates(), 1);
        let mut nobody = manager();
        assert_eq!(nobody.candidates(), 0);
        assert_eq!(nobody.standing(), Standing::None);
        assert!(nobody.tick(0).is_empty());
    }

    #[test]
    fn static_relays_are_asked_before_learned_ones_and_learned_only_fill_the_gap() {
        // RELAY.md section 3: static candidates have precedence; learned
        // ones are considered only when the static ones cannot satisfy
        // the target.
        let mut m = with_static(&[R1]);
        assert!(m.learn(ident(R2), "/ip4/192.0.2.9/tcp/1"));
        assert!(m.learn(ident(R3), "/ip4/192.0.2.10/tcp/1"));
        let asked = reserves(&m.tick(0));
        assert_eq!(asked.len(), 2, "target 2 from three candidates");
        assert_eq!(asked[0], ident(R1), "the static relay first");
        assert_eq!(
            m.source(&asked[1]),
            Some(RelaySource::Learned),
            "then one learned"
        );
        // The other learned relay is promoted to static later: when the
        // asked learned one is lost, the promoted one is asked next --
        // and it would have been asked anyway as the only idle one, so
        // the control is the order when both are idle, below.
        let learned = asked[1].clone();
        let other = if learned == ident(R2) {
            ident(R3)
        } else {
            ident(R2)
        };
        assert!(m.add_static(other.clone(), "/ip4/192.0.2.2/tcp/1"));
        assert_eq!(m.source(&other), Some(RelaySource::Static), "promoted");
        let _ = m.record_failed(&learned, 1, 0);
        assert_eq!(reserves(&m.tick(2)), vec![other.clone()]);
        // THE ORDER: with the promoted static and the learned relay both
        // idle again, the static one is asked first.
        let _ = m.record_failed(&other, 3, 0);
        let mut fresh = with_static(&[R1]);
        assert!(fresh.learn(ident(R2), "/ip4/192.0.2.9/tcp/1"));
        assert!(fresh.add_static(ident(R3), "/ip4/192.0.2.3/tcp/1"));
        assert_eq!(
            reserves(&fresh.tick(0)),
            vec![ident(R1), ident(R3)],
            "both static before the learned"
        );
    }

    #[test]
    fn an_acceptance_advertises_the_address_and_a_loss_withdraws_it_in_the_same_call() {
        // RELAY.md section 5: add only after acceptance; remove
        // immediately on loss.
        let mut m = with_static(&[R1, R2]);
        let _ = m.tick(0);
        assert!(m.advertised().is_empty(), "requested is not active");
        let a1 = format!("/ip4/192.0.2.1/tcp/4001/p2p/{R1}/p2p-circuit");
        let a2 = format!("/ip4/192.0.2.2/tcp/4001/p2p/{R2}/p2p-circuit");
        assert_eq!(m.record_accepted(&ident(R1), &a1, 10), Ok(None));
        assert_eq!(m.record_accepted(&ident(R2), &a2, 11), Ok(None));
        assert_eq!(m.advertised(), vec![a1.clone(), a2.clone()]);
        assert_eq!(m.standing(), Standing::Satisfied);
        // A renewal keeps it active and keeps its acceptance time.
        assert_eq!(m.record_accepted(&ident(R1), &a1, 500), Ok(None));
        assert_eq!(m.active(), 2);
        assert!(matches!(
            m.state(&ident(R1)),
            Some(ReservationState::Active { since_ms: 10, .. })
        ));
        // A renewal through a different address supersedes the one
        // advertised: it comes back to be withdrawn, and only the new
        // one is in the set.
        let a1b = format!("/ip4/192.0.2.1/tcp/4002/p2p/{R1}/p2p-circuit");
        assert_eq!(
            m.record_accepted(&ident(R1), &a1b, 501),
            Ok(Some(a1.clone()))
        );
        assert_eq!(m.advertised(), vec![a1b.clone(), a2.clone()]);
        assert_eq!(m.record_accepted(&ident(R1), &a1, 502), Ok(Some(a1b)));
        // The loss: the address comes back to be withdrawn and is gone
        // from the set before this call returns.
        assert_eq!(m.record_failed(&ident(R1), 600, 0), Ok(Some(a1)));
        assert_eq!(m.advertised(), vec![a2]);
        assert_eq!(m.standing(), Standing::Partial);
        assert!(matches!(
            m.state(&ident(R1)),
            Some(ReservationState::Backoff { attempts: 1, .. })
        ));
    }

    #[test]
    fn a_report_from_a_stranger_or_for_an_unasked_relay_is_refused_by_name() {
        let mut m = with_static(&[R1, R2]);
        let a = "/ip4/192.0.2.9/tcp/1/p2p-circuit";
        assert_eq!(
            m.record_accepted(&ident(R3), a, 0),
            Err(RefusedRelayReport::UnknownRelay),
            "never offered"
        );
        assert_eq!(
            m.record_accepted(&ident(R1), a, 0),
            Err(RefusedRelayReport::UnrequestedAcceptance),
            "offered, never asked"
        );
        assert!(
            m.advertised().is_empty(),
            "and nothing was advertised for either"
        );
        assert_eq!(
            m.record_failed(&ident(R3), 0, 0),
            Err(RefusedRelayReport::UnknownRelay)
        );
        // An acceptance with nothing to advertise.
        assert_eq!(
            m.record_accepted(&ident(R2), "", 0),
            Err(RefusedRelayReport::EmptyAddress)
        );
        // A failure for an idle relay: the echo of a release, or a
        // report about a relay nothing asked. Not backed off.
        assert_eq!(
            m.record_failed(&ident(R1), 0, 0),
            Err(RefusedRelayReport::UnrequestedFailure)
        );
        assert_eq!(m.state(&ident(R1)), Some(&ReservationState::Idle));
        // Backing off is not asked either.
        let _ = m.tick(0);
        let _ = m.record_failed(&ident(R1), 1, 0);
        assert_eq!(
            m.record_accepted(&ident(R1), a, 2),
            Err(RefusedRelayReport::UnrequestedAcceptance)
        );
        assert!(m.advertised().is_empty(), "still nothing advertised");
    }

    #[test]
    fn a_released_reservation_whose_close_is_reported_back_is_not_backed_off() {
        // The adapter closes the listener a Release names; libp2p
        // reports the close as any other; the adapter maps it to a
        // failure. The relay did nothing wrong and is askable the
        // moment the target rises again.
        let mut m = with_static(&[R1, R2]);
        let _ = m.tick(0);
        assert_eq!(m.record_accepted(&ident(R1), "/circuit/1", 1), Ok(None));
        assert_eq!(m.record_accepted(&ident(R2), "/circuit/2", 2), Ok(None));
        let released = m.set_direct_inbound(DirectInboundState::VerifiedPublic);
        assert_eq!(released.len(), 1);
        let Action::Release { relay, .. } = &released[0] else {
            panic!("a release");
        };
        assert_eq!(
            m.record_failed(relay, 3, 0),
            Err(RefusedRelayReport::UnrequestedFailure)
        );
        let _ = m.set_direct_inbound(DirectInboundState::NotVerified);
        assert_eq!(
            reserves(&m.tick(4)),
            vec![relay.clone()],
            "asked again at once, not after retry_min"
        );
    }

    #[test]
    fn a_failed_relay_backs_off_on_its_own_ladder_and_a_success_resets_only_it() {
        // RELAY.md section 4: bounded backoff, 5 s doubling to 5 min,
        // per relay; "do not hammer one failed relay".
        let config = ReservationConfig::default();
        assert_eq!(config.retry_delay_ms(0), 5_000);
        assert_eq!(config.retry_delay_ms(1), 10_000);
        assert_eq!(config.retry_delay_ms(5), 160_000);
        assert_eq!(config.retry_delay_ms(6), 300_000, "capped at the maximum");
        assert_eq!(
            config.retry_delay_ms(40),
            300_000,
            "and the exponent is clamped"
        );

        let mut m = with_static(&[R1, R2]);
        let _ = m.tick(0);
        let _ = m.record_failed(&ident(R1), 0, 0);
        assert!(matches!(
            m.state(&ident(R1)),
            Some(ReservationState::Backoff {
                until_ms: 5_000,
                attempts: 1
            })
        ));
        assert!(
            reserves(&m.tick(4_999)).is_empty(),
            "not asked before its delay"
        );
        assert_eq!(reserves(&m.tick(5_000)), vec![ident(R1)], "asked when due");
        let _ = m.record_failed(&ident(R1), 5_000, 0);
        assert!(matches!(
            m.state(&ident(R1)),
            Some(ReservationState::Backoff {
                until_ms: 15_000,
                attempts: 2
            })
        ));
        // R2 is untouched by R1's failures: still requested since tick 0.
        assert!(matches!(
            m.state(&ident(R2)),
            Some(ReservationState::Requested { since_ms: 0, .. })
        ));
        // A success resets R1's ladder: the next failure starts at the
        // minimum again.
        let _ = m.tick(15_000);
        assert_eq!(
            m.record_accepted(&ident(R1), "/ip4/192.0.2.1/tcp/1/p2p-circuit", 15_001),
            Ok(None)
        );
        let _ = m.record_failed(&ident(R1), 20_000, 0);
        assert!(matches!(
            m.state(&ident(R1)),
            Some(ReservationState::Backoff {
                until_ms: 25_000,
                attempts: 1
            })
        ));
    }

    #[test]
    fn jitter_is_added_and_capped_at_the_delay() {
        let mut m = with_static(&[R1]);
        let _ = m.tick(0);
        let _ = m.record_failed(&ident(R1), 0, 1_234);
        assert!(matches!(
            m.state(&ident(R1)),
            Some(ReservationState::Backoff {
                until_ms: 6_234,
                ..
            })
        ));
        let _ = m.tick(6_234);
        let _ = m.record_failed(&ident(R1), 6_234, u64::MAX);
        assert!(
            matches!(
                m.state(&ident(R1)),
                Some(ReservationState::Backoff {
                    until_ms: 26_234,
                    attempts: 2
                })
            ),
            "10 s delay plus at most 10 s of jitter"
        );
    }

    #[test]
    fn a_target_that_fell_releases_the_surplus_learned_first_newest_first() {
        // Two static, one learned, all active while private; then the
        // verdict becomes public and the target is one: the learned
        // reservation goes first, then the newer static one, and the
        // oldest static relay stays warm.
        let mut m = ReservationManager::new(ReservationConfig {
            max_reservations: 4,
            target_private_or_unknown: 3,
            target_public: 1,
            ..ReservationConfig::default()
        })
        .expect("valid");
        assert!(m.add_static(ident(R1), "/ip4/192.0.2.1/tcp/1"));
        assert!(m.add_static(ident(R2), "/ip4/192.0.2.2/tcp/1"));
        assert!(m.learn(ident(R3), "/ip4/192.0.2.3/tcp/1"));
        assert_eq!(reserves(&m.tick(0)).len(), 3);
        for (r, t) in [(R1, 10), (R2, 20), (R3, 30)] {
            assert_eq!(
                m.record_accepted(&ident(r), &format!("/circuit/{r}"), t),
                Ok(None)
            );
        }
        assert_eq!(m.active(), 3);
        // R1 renews last: age is the acceptance, not the last renewal,
        // so it is still the oldest.
        assert_eq!(
            m.record_accepted(&ident(R1), &format!("/circuit/{R1}"), 3_600),
            Ok(None)
        );
        let released = m.set_direct_inbound(DirectInboundState::VerifiedPublic);
        assert_eq!(
            released,
            vec![
                Action::Release {
                    relay: ident(R3),
                    address: format!("/circuit/{R3}"),
                },
                Action::Release {
                    relay: ident(R2),
                    address: format!("/circuit/{R2}"),
                },
            ]
        );
        assert_eq!(m.advertised(), vec![format!("/circuit/{R1}")]);
        assert_eq!(m.standing(), Standing::Satisfied);
        // And back to private: the two are asked again.
        let _ = m.set_direct_inbound(DirectInboundState::NotVerified);
        assert_eq!(reserves(&m.tick(100)).len(), 2);
    }

    #[test]
    fn a_forgotten_relay_returns_its_address_and_is_never_asked_again() {
        // RELAY.md section 5: removed when the relay becomes
        // unauthorized.
        let mut m = with_static(&[R1, R2]);
        let _ = m.tick(0);
        assert_eq!(m.record_accepted(&ident(R1), "/circuit/1", 1), Ok(None));
        assert_eq!(m.forget(&ident(R1)), Some("/circuit/1".to_owned()));
        assert!(m.advertised().is_empty());
        assert_eq!(m.forget(&ident(R1)), None, "already gone");
        assert_eq!(m.forget(&ident(R2)), None, "requested, nothing advertised");
        assert!(m.tick(2).is_empty(), "no candidates left to ask");
        assert_eq!(m.state(&ident(R1)), None);
    }

    #[test]
    fn the_candidate_sets_are_bounded_and_a_promotion_counts_against_the_static_bound() {
        let mut m = manager();
        for i in 0..MAX_STATIC_RELAYS {
            assert!(m.add_static(nth(i), "/ip4/192.0.2.1/tcp/1"), "static {i}");
        }
        assert!(
            !m.add_static(nth(MAX_STATIC_RELAYS), "/ip4/192.0.2.1/tcp/1"),
            "the seventeenth static relay is refused"
        );
        assert!(
            m.add_static(nth(0), "/ip4/192.0.2.1/tcp/2"),
            "a second address for a known relay is not a new relay"
        );
        for i in 0..MAX_LEARNED_RELAYS {
            assert!(m.learn(nth(100 + i), "/ip4/192.0.2.1/tcp/1"), "learned {i}");
        }
        assert!(
            !m.learn(nth(100 + MAX_LEARNED_RELAYS), "/ip4/192.0.2.1/tcp/1"),
            "the seventeenth learned relay is refused"
        );
        assert!(!m.learn(nth(200), ""), "and an empty address is refused");
        assert!(!m.add_static(nth(200), ""));
        // A learned relay offered as static while the static set is
        // full is refused, and stays learned: a promotion is a
        // seventeenth static relay like any other.
        assert!(!m.add_static(nth(100), "/ip4/192.0.2.1/tcp/9"));
        assert_eq!(m.source(&nth(100)), Some(RelaySource::Learned));
        assert_eq!(m.count(RelaySource::Static), MAX_STATIC_RELAYS);
        assert_eq!(m.candidates(), MAX_STATIC_RELAYS + MAX_LEARNED_RELAYS);
        // With room, the promotion keeps the relay's state and address
        // list and does not duplicate it.
        let mut room = manager();
        assert!(room.learn(nth(1), "/ip4/192.0.2.1/tcp/1"));
        let _ = room.tick(0);
        assert!(room.add_static(nth(1), "/ip4/192.0.2.1/tcp/9"));
        assert_eq!(room.source(&nth(1)), Some(RelaySource::Static));
        assert!(matches!(
            room.state(&nth(1)),
            Some(ReservationState::Requested { .. })
        ));
        assert_eq!(room.candidates(), 1);
    }

    #[test]
    fn a_relay_keeps_at_most_eight_addresses_and_a_static_one_keeps_only_the_operators() {
        // Identify is pushed by the peer as often as it likes, each
        // push carrying whatever addresses it cares to claim: the list
        // a learned relay is dialled at is bounded, and a static relay
        // is dialled only where the operator said.
        let mut m = manager();
        for i in 0..MAX_ADDRESSES_PER_RELAY {
            assert!(m.learn(ident(R1), &format!("/ip4/192.0.2.1/tcp/{i}")));
        }
        assert!(
            !m.learn(ident(R1), "/ip4/192.0.2.1/tcp/99"),
            "the ninth learned address is refused"
        );
        assert!(
            m.learn(ident(R1), "/ip4/192.0.2.1/tcp/0"),
            "a known one is not refused"
        );
        assert!(m.add_static(ident(R2), "/ip4/192.0.2.2/tcp/1"));
        for i in 2..=MAX_ADDRESSES_PER_RELAY {
            assert!(m.add_static(ident(R2), &format!("/ip4/192.0.2.2/tcp/{i}")));
        }
        assert!(
            !m.add_static(ident(R2), "/ip4/192.0.2.2/tcp/99"),
            "the ninth configured address is refused"
        );
        assert!(m.add_static(ident(R3), "/ip4/192.0.2.3/tcp/1"));
        assert!(
            m.learn(ident(R3), "/ip4/203.0.113.9/tcp/1"),
            "known: not refused, and not added either"
        );
        let asked = m.tick(0);
        let addresses = |relay: &TransportIdentity| {
            asked
                .iter()
                .find_map(|a| match a {
                    Action::Reserve {
                        relay: r,
                        addresses,
                    } if r == relay => Some(addresses.clone()),
                    _ => None,
                })
                .expect("asked")
        };
        assert_eq!(addresses(&ident(R2)).len(), MAX_ADDRESSES_PER_RELAY);
        assert_eq!(
            addresses(&ident(R3)),
            vec!["/ip4/192.0.2.3/tcp/1".to_owned()],
            "the static relay's list is the operator's"
        );
    }

    #[test]
    fn a_tick_asks_only_up_to_the_target_counting_what_is_already_requested() {
        let mut m = with_static(&[R1, R2, R3]);
        assert_eq!(reserves(&m.tick(0)).len(), 2);
        assert!(
            reserves(&m.tick(1)).is_empty(),
            "two requested: nothing more"
        );
        assert_eq!(m.record_accepted(&ident(R1), "/circuit/1", 2), Ok(None));
        assert!(reserves(&m.tick(3)).is_empty(), "one active, one requested");
        let _ = m.record_failed(&ident(R2), 4, 0);
        assert_eq!(
            reserves(&m.tick(5)),
            vec![ident(R3)],
            "the gap is filled from the third"
        );
    }
}
