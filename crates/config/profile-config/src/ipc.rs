// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The `ipc` block (plan §16 (13)): whether the local IPC boundary runs,
//! its socket layout, its client ceilings, each client's event queue and
//! the keepalive -- with the block's four cross-field rules. The two
//! rules tying it to `runtime.deployment` are the runtime block's
//! ([`crate::runtime`]), which reads `enabled` from here.
//!
//! What is NOT here, on purpose: the IPC protocol version (negotiated in
//! `hello`) and the body ceiling (a v2 protocol constant) are not
//! operator settings (`config.schema.yaml`).

use serde::{Deserialize, Serialize};

use crate::{ConfigError, de_duration_ms, ser_duration_ms};

/// The client's silence bound, `CLIENT_SILENCE_TIMEOUT` in
/// `LOCAL-IPC.md` (A 2026-10-08), in milliseconds: a client armed by a
/// ping ends the connection after this long without reading a frame, so
/// a profile's `interval + response_timeout` must not exceed it. Stated
/// here from the contract rather than imported, since this crate does not
/// depend on the IPC protocol; a test holds it equal to the protocol's
/// `CLIENT_SILENCE_TIMEOUT` (`the_bound_is_the_protocols_client_silence_timeout`).
pub const CLIENT_SILENCE_TIMEOUT_MS: u32 = 120_000;

/// `ipc.socket_layout`: `literal[split-data-admin]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum SocketLayout {
    /// Separate data and admin sockets: authority is a filesystem fact,
    /// never a claim a data client makes (ADR-0037).
    #[default]
    #[serde(rename = "split-data-admin")]
    SplitDataAdmin,
}

/// `ipc.keepalive`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct KeepaliveConfig {
    /// `bool = true`.
    pub enabled: bool,
    /// `bool = true`: a client claiming an endpoint lease must negotiate
    /// the keepalive feature in `hello`.
    pub require_for_endpoint_lease: bool,
    /// `duration[10s..5m] = 30s`, in milliseconds.
    #[serde(
        rename = "interval",
        deserialize_with = "de_duration_ms",
        serialize_with = "ser_duration_ms"
    )]
    pub interval_ms: u32,
    /// `duration[2s..1m] = 10s`, in milliseconds.
    #[serde(
        rename = "response_timeout",
        deserialize_with = "de_duration_ms",
        serialize_with = "ser_duration_ms"
    )]
    pub response_timeout_ms: u32,
    /// `integer[1..10] = 3`.
    pub max_missed: u32,
}

impl Default for KeepaliveConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            require_for_endpoint_lease: true,
            interval_ms: 30_000,
            response_timeout_ms: 10_000,
            max_missed: 3,
        }
    }
}

/// `ipc`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct IpcConfig {
    /// `bool = true`: `daemon-ipc` needs it, `embedded-android` refuses
    /// it (the runtime rules).
    pub enabled: bool,
    /// The socket layout.
    pub socket_layout: SocketLayout,
    /// `integer[1..64] = 16`.
    pub max_clients: u32,
    /// `integer[1..16] = 4`.
    pub max_admin_clients: u32,
    /// `integer[16..1024] = 256`: each client's event queue.
    pub client_event_queue: u32,
    /// The keepalive.
    pub keepalive: KeepaliveConfig,
}

impl Default for IpcConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            socket_layout: SocketLayout::SplitDataAdmin,
            max_clients: 16,
            max_admin_clients: 4,
            client_event_queue: 256,
            keepalive: KeepaliveConfig::default(),
        }
    }
}

impl IpcConfig {
    /// The block's ranges and its four cross-field rules.
    pub(crate) fn validate_into(&self, errors: &mut Vec<ConfigError>) {
        let k = &self.keepalive;
        let rows: [(&'static str, u32, u32, u32); 6] = [
            ("ipc.max_clients", self.max_clients, 1, 64),
            ("ipc.max_admin_clients", self.max_admin_clients, 1, 16),
            ("ipc.client_event_queue", self.client_event_queue, 16, 1_024),
            ("ipc.keepalive.interval", k.interval_ms, 10_000, 300_000),
            (
                "ipc.keepalive.response_timeout",
                k.response_timeout_ms,
                2_000,
                60_000,
            ),
            ("ipc.keepalive.max_missed", k.max_missed, 1, 10),
        ];
        for (field, got, lo, hi) in rows {
            if !(lo..=hi).contains(&got) {
                errors.push(ConfigError::OutOfRange {
                    field,
                    got: u64::from(got),
                    allowed: (u64::from(lo), u64::from(hi)),
                });
            }
        }
        // Admin capability is selected by the admin socket, so its
        // clients are a subset of all clients, never more.
        if self.max_admin_clients > self.max_clients {
            errors.push(ConfigError::OrderViolated {
                lesser: "ipc.max_admin_clients",
                lesser_got: u64::from(self.max_admin_clients),
                greater: "ipc.max_clients",
                greater_got: u64::from(self.max_clients),
                strict: false,
            });
        }
        // A probe must be answered before the next one is due, or
        // max_missed counts intervals rather than missed answers.
        if k.enabled && k.response_timeout_ms >= k.interval_ms {
            errors.push(ConfigError::OrderViolated {
                lesser: "ipc.keepalive.response_timeout",
                lesser_got: u64::from(k.response_timeout_ms),
                greater: "ipc.keepalive.interval",
                greater_got: u64::from(k.interval_ms),
                strict: true,
            });
        }
        // A healthy server's next ping lands inside the bound a client
        // armed at the previous one: with the keepalive off nothing
        // pings and nothing arms (`a_ping_lands_inside_the_clients_silence_bound`).
        let sum = u64::from(k.interval_ms) + u64::from(k.response_timeout_ms);
        if k.enabled && sum > u64::from(CLIENT_SILENCE_TIMEOUT_MS) {
            errors.push(ConfigError::KeepaliveOutlastsClientSilence {
                interval_ms: u64::from(k.interval_ms),
                response_timeout_ms: u64::from(k.response_timeout_ms),
                limit_ms: u64::from(CLIENT_SILENCE_TIMEOUT_MS),
            });
        }
        // A lease cannot require what is switched off.
        if k.require_for_endpoint_lease && !k.enabled {
            errors.push(ConfigError::KeepaliveRequiredButDisabled);
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]
    use super::*;

    fn parse(yaml: &str) -> IpcConfig {
        serde_norway::from_str(yaml).expect("parses")
    }

    fn errors_of(ipc: &IpcConfig) -> Vec<ConfigError> {
        let mut errors = Vec::new();
        ipc.validate_into(&mut errors);
        errors
    }

    #[test]
    fn the_defaults_are_the_schemas_and_are_valid() {
        let ipc = parse("{}");
        assert_eq!(ipc, IpcConfig::default());
        assert!(ipc.enabled && ipc.keepalive.enabled && ipc.keepalive.require_for_endpoint_lease);
        assert_eq!(
            (
                ipc.max_clients,
                ipc.max_admin_clients,
                ipc.client_event_queue
            ),
            (16, 4, 256)
        );
        assert_eq!(
            (
                ipc.keepalive.interval_ms,
                ipc.keepalive.response_timeout_ms,
                ipc.keepalive.max_missed
            ),
            (30_000, 10_000, 3)
        );
        assert!(errors_of(&ipc).is_empty());
    }

    #[test]
    fn every_range_is_checked_at_both_edges_by_name() {
        let at_lo = parse(
            "{max_clients: 1, max_admin_clients: 1, client_event_queue: 16, keepalive: {interval: 10s, response_timeout: 2s, max_missed: 1}}",
        );
        let at_hi = parse(
            "{max_clients: 64, max_admin_clients: 16, client_event_queue: 1024, keepalive: {interval: 5m, response_timeout: 1m, max_missed: 10}}",
        );
        assert!(errors_of(&at_lo).is_empty(), "{:?}", errors_of(&at_lo));
        // Every upper edge is in range on its own; together the keepalive
        // pair breaks the client's silence bound, and nothing else.
        assert_eq!(
            errors_of(&at_hi),
            vec![ConfigError::KeepaliveOutlastsClientSilence {
                interval_ms: 300_000,
                response_timeout_ms: 60_000,
                limit_ms: 120_000,
            }]
        );
        let fields = |ipc: &IpcConfig| -> Vec<&'static str> {
            errors_of(ipc)
                .into_iter()
                .filter_map(|e| match e {
                    ConfigError::OutOfRange { field, .. } => Some(field),
                    _ => None,
                })
                .collect()
        };
        let expected = [
            "ipc.max_clients",
            "ipc.max_admin_clients",
            "ipc.client_event_queue",
            "ipc.keepalive.interval",
            "ipc.keepalive.response_timeout",
            "ipc.keepalive.max_missed",
        ];
        let below = parse(
            "{max_clients: 0, max_admin_clients: 0, client_event_queue: 15, keepalive: {interval: 9s, response_timeout: 1999ms, max_missed: 0}}",
        );
        let above = parse(
            "{max_clients: 65, max_admin_clients: 17, client_event_queue: 1025, keepalive: {interval: 301s, response_timeout: 61s, max_missed: 11}}",
        );
        assert_eq!(fields(&below), expected);
        assert_eq!(fields(&above), expected);
    }

    /// `max_admin_clients <= max_clients`: equality allowed, one more not.
    #[test]
    fn admin_clients_never_outnumber_clients() {
        assert!(errors_of(&parse("{max_clients: 4, max_admin_clients: 4}")).is_empty());
        assert_eq!(
            errors_of(&parse("{max_clients: 4, max_admin_clients: 5}")),
            vec![ConfigError::OrderViolated {
                lesser: "ipc.max_admin_clients",
                lesser_got: 5,
                greater: "ipc.max_clients",
                greater_got: 4,
                strict: false,
            }]
        );
    }

    /// `response_timeout < interval` while the keepalive runs: equality
    /// is refused, and a keepalive that is off is not judged.
    #[test]
    fn a_probe_is_answered_before_the_next_is_due() {
        assert!(
            errors_of(&parse(
                "{keepalive: {interval: 20s, response_timeout: 19s}}"
            ))
            .is_empty()
        );
        assert_eq!(
            errors_of(&parse(
                "{keepalive: {interval: 20s, response_timeout: 20s}}"
            )),
            vec![ConfigError::OrderViolated {
                lesser: "ipc.keepalive.response_timeout",
                lesser_got: 20_000,
                greater: "ipc.keepalive.interval",
                greater_got: 20_000,
                strict: true,
            }]
        );
        assert!(
            errors_of(&parse(
                "{keepalive: {enabled: false, require_for_endpoint_lease: false, interval: 20s, response_timeout: 30s}}"
            ))
            .is_empty(),
            "a keepalive that is off is not judged"
        );
    }

    /// The profile's copy of the bound is the IPC protocol's, the one the
    /// client's timer arms on.
    #[test]
    fn the_bound_is_the_protocols_client_silence_timeout() {
        assert_eq!(
            u128::from(CLIENT_SILENCE_TIMEOUT_MS),
            interweave_ipc_protocol::CLIENT_SILENCE_TIMEOUT.as_millis()
        );
    }

    /// `interval + response_timeout` at exactly the client's silence
    /// bound passes, one millisecond over is refused naming both values,
    /// and a keepalive that is off is not judged (`LOCAL-IPC.md`, A
    /// 2026-10-08).
    #[test]
    fn a_ping_lands_inside_the_clients_silence_bound() {
        assert_eq!(CLIENT_SILENCE_TIMEOUT_MS, 120_000);
        assert!(
            errors_of(&parse(
                "{keepalive: {interval: 100000ms, response_timeout: 20000ms}}"
            ))
            .is_empty(),
            "exactly the bound"
        );
        assert_eq!(
            errors_of(&parse(
                "{keepalive: {interval: 100001ms, response_timeout: 20000ms}}"
            )),
            vec![ConfigError::KeepaliveOutlastsClientSilence {
                interval_ms: 100_001,
                response_timeout_ms: 20_000,
                limit_ms: 120_000,
            }]
        );
        assert!(
            errors_of(&parse(
                "{keepalive: {enabled: false, require_for_endpoint_lease: false, interval: 300s, response_timeout: 60s}}"
            ))
            .is_empty(),
            "a keepalive that is off is not judged"
        );
    }

    #[test]
    fn a_lease_cannot_require_a_keepalive_that_is_off() {
        assert_eq!(
            errors_of(&parse("{keepalive: {enabled: false}}")),
            vec![ConfigError::KeepaliveRequiredButDisabled],
            "require_for_endpoint_lease defaults to true"
        );
        assert!(
            errors_of(&parse(
                "{keepalive: {enabled: false, require_for_endpoint_lease: false}}"
            ))
            .is_empty()
        );
    }

    #[test]
    fn the_layout_is_a_literal_and_unknown_keys_are_refused() {
        assert_eq!(
            parse("{socket_layout: split-data-admin}").socket_layout,
            SocketLayout::SplitDataAdmin
        );
        for refused in [
            "{socket_layout: single}",
            "{version: 2}",
            "{max_body_bytes: 131072}",
            "{keepalive: {period: 30s}}",
        ] {
            assert!(
                serde_norway::from_str::<IpcConfig>(refused).is_err(),
                "{refused} parsed"
            );
        }
    }
}
