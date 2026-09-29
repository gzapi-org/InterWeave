// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The `transport` block beside `connectivity`: `backend`, `listen`,
//! `limits`, `pre_auth`, `connection_policy`, `direct` and `pubsub`, as
//! `config.schema.yaml` declares them (plan §16 (13)).
//!
//! Every block and every field is defaulted to the schema's default, so a
//! profile that states none of them means exactly what the schema says;
//! a `literal[...]` string is a one-variant enum, so any other spelling
//! is refused where it is parsed, and a `literal[true]` flag is a `bool`
//! whose other value `validate_into` refuses by name. Ranges are checked
//! in `validate_into`, never by the deserializer, so every violation in a
//! document is reported at once.

use serde::{Deserialize, Serialize};

use crate::{ConfigError, de_duration_ms, ser_duration_ms};

/// `transport.backend`: `enum[libp2p]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TransportBackend {
    /// The one backend.
    #[default]
    Libp2p,
}

/// `transport.listen`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ListenConfig {
    /// `list[string, max=16]`: multiaddrs to bind. Parsed as addresses
    /// where the runtime binds them, not here: this crate names no
    /// backend type.
    #[serde(default)]
    pub addresses: Vec<String>,
}

/// The most listen addresses a profile may name.
pub const MAX_LISTEN_ADDRESSES: usize = 16;

/// `transport.limits`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct LimitsConfig {
    /// `integer[1024..49152] = 49152`.
    pub max_payload_bytes: u32,
    /// `integer[1..2048] = 256`.
    pub max_connected_peers: u32,
    /// `integer[1..4096] = 384`.
    pub max_connections_total: u32,
    /// `integer[1..8] = 3`.
    pub max_connections_per_peer: u32,
    /// `integer[1..16384] = 4096`.
    pub max_candidates: u32,
    /// `integer[1..32] = 16`.
    pub max_addresses_per_peer: u32,
    /// `integer[1..1024] = 128`.
    pub max_subscriptions: u32,
}

impl Default for LimitsConfig {
    fn default() -> Self {
        Self {
            max_payload_bytes: 49_152,
            max_connected_peers: 256,
            max_connections_total: 384,
            max_connections_per_peer: 3,
            max_candidates: 4_096,
            max_addresses_per_peer: 16,
            max_subscriptions: 128,
        }
    }
}

/// `transport.pre_auth`: bounds that apply before a remote `PeerId`
/// exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct PreAuthConfig {
    /// `duration[5s..30s] = 10s`, in milliseconds.
    #[serde(
        rename = "handshake_timeout",
        deserialize_with = "de_duration_ms",
        serialize_with = "ser_duration_ms"
    )]
    pub handshake_timeout_ms: u32,
    /// `integer[1..256] = 64`.
    pub max_pending_inbound_handshakes: u32,
    /// `integer[1..32] = 8`.
    pub max_pending_per_source_bucket: u32,
    /// `integer[1..600] = 30`.
    pub max_attempts_per_source_bucket_per_minute: u32,
    /// `integer[1..6000] = 600`.
    pub max_attempts_global_per_minute: u32,
    /// `literal[64] = 64`.
    pub ipv6_source_prefix_bits: u32,
}

impl Default for PreAuthConfig {
    fn default() -> Self {
        Self {
            handshake_timeout_ms: 10_000,
            max_pending_inbound_handshakes: 64,
            max_pending_per_source_bucket: 8,
            max_attempts_per_source_bucket_per_minute: 30,
            max_attempts_global_per_minute: 600,
            ipv6_source_prefix_bits: 64,
        }
    }
}

/// `transport.connection_policy`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ConnectionPolicyConfig {
    /// `duration[1s..1m] = 5s`, in milliseconds.
    #[serde(
        rename = "address_backoff_min",
        deserialize_with = "de_duration_ms",
        serialize_with = "ser_duration_ms"
    )]
    pub address_backoff_min_ms: u32,
    /// `duration[30s..30m] = 5m`, in milliseconds.
    #[serde(
        rename = "address_backoff_max",
        deserialize_with = "de_duration_ms",
        serialize_with = "ser_duration_ms"
    )]
    pub address_backoff_max_ms: u32,
    /// `duration[1m..24h] = 30m`, in milliseconds.
    #[serde(
        rename = "identity_mismatch_quarantine",
        deserialize_with = "de_duration_ms",
        serialize_with = "ser_duration_ms"
    )]
    pub identity_mismatch_quarantine_ms: u32,
}

impl Default for ConnectionPolicyConfig {
    fn default() -> Self {
        Self {
            address_backoff_min_ms: 5_000,
            address_backoff_max_ms: 300_000,
            identity_mismatch_quarantine_ms: 1_800_000,
        }
    }
}

/// An `inbound_rate_limit` block: `direct` and `pubsub` each carry one,
/// accounted separately.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct InboundRateLimitConfig {
    /// `literal[true] = true`.
    pub enabled: bool,
    /// `integer[1..6000] = 120`.
    pub per_peer_per_minute: u32,
    /// `integer[1..512] = 32`.
    pub per_peer_burst: u32,
    /// `integer[1..60000] = 1200`.
    pub global_per_minute: u32,
    /// `integer[1..2048] = 256`.
    pub global_burst: u32,
}

impl Default for InboundRateLimitConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            per_peer_per_minute: 120,
            per_peer_burst: 32,
            global_per_minute: 1_200,
            global_burst: 256,
        }
    }
}

/// `transport.direct.protocol`: `literal[/interweave/direct/2.0.0]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum DirectProtocol {
    /// The one directed protocol (CLAUDE.md §5).
    #[default]
    #[serde(rename = "/interweave/direct/2.0.0")]
    DirectV2,
}

/// `transport.direct`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct DirectConfig {
    /// The directed protocol.
    pub protocol: DirectProtocol,
    /// `integer[1000..60000] = 10000`.
    pub timeout_ms: u32,
    /// `integer[1..512] = 128`.
    pub max_inflight_total: u32,
    /// `integer[1..32] = 8`.
    pub max_inflight_per_peer: u32,
    /// The direct-ingress budget.
    pub inbound_rate_limit: InboundRateLimitConfig,
}

impl Default for DirectConfig {
    fn default() -> Self {
        Self {
            protocol: DirectProtocol::DirectV2,
            timeout_ms: 10_000,
            max_inflight_total: 128,
            max_inflight_per_peer: 8,
            inbound_rate_limit: InboundRateLimitConfig::default(),
        }
    }
}

/// `transport.pubsub.backend`: `enum[gossipsub]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PubsubBackend {
    /// Signed GossipSub (CLAUDE.md §5).
    #[default]
    Gossipsub,
}

/// `transport.pubsub.message_id_function`:
/// `literal[source-peer-seqno-sha256-v1]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum MessageIdFunction {
    /// The signed source `PeerId` plus the wire sequence number, never an
    /// application envelope id.
    #[default]
    #[serde(rename = "source-peer-seqno-sha256-v1")]
    SourcePeerSeqnoSha256V1,
}

/// `transport.pubsub`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct PubsubConfig {
    /// The broadcast backend.
    pub backend: PubsubBackend,
    /// `literal[true]`.
    pub signed_messages: bool,
    /// `literal[true]`.
    pub strict_validation: bool,
    /// `literal[true]`.
    pub explicit_application_validation: bool,
    /// The mesh duplicate identity.
    pub message_id_function: MessageIdFunction,
    /// The broadcast-ingress budget, separate from `direct`'s.
    pub inbound_rate_limit: InboundRateLimitConfig,
}

impl Default for PubsubConfig {
    fn default() -> Self {
        Self {
            backend: PubsubBackend::Gossipsub,
            signed_messages: true,
            strict_validation: true,
            explicit_application_validation: true,
            message_id_function: MessageIdFunction::SourcePeerSeqnoSha256V1,
            inbound_rate_limit: InboundRateLimitConfig::default(),
        }
    }
}

/// One range row: the dotted field path, the value, and the inclusive
/// bounds the schema states.
type Row = (&'static str, u64, u64, u64);

fn rate_rows(prefix: &'static [&'static str; 4], r: &InboundRateLimitConfig) -> [Row; 4] {
    [
        (prefix[0], u64::from(r.per_peer_per_minute), 1, 6_000),
        (prefix[1], u64::from(r.per_peer_burst), 1, 512),
        (prefix[2], u64::from(r.global_per_minute), 1, 60_000),
        (prefix[3], u64::from(r.global_burst), 1, 2_048),
    ]
}

/// Check every range and literal of the blocks this module models.
pub(crate) fn validate_into(
    listen: &ListenConfig,
    limits: &LimitsConfig,
    pre_auth: &PreAuthConfig,
    policy: &ConnectionPolicyConfig,
    direct: &DirectConfig,
    pubsub: &PubsubConfig,
    errors: &mut Vec<ConfigError>,
) {
    if listen.addresses.len() > MAX_LISTEN_ADDRESSES {
        errors.push(ConfigError::OutOfRange {
            field: "transport.listen.addresses",
            got: u64::try_from(listen.addresses.len()).unwrap_or(u64::MAX),
            allowed: (0, MAX_LISTEN_ADDRESSES as u64),
        });
    }
    let mut rows: Vec<Row> = vec![
        (
            "transport.limits.max_payload_bytes",
            u64::from(limits.max_payload_bytes),
            1_024,
            49_152,
        ),
        (
            "transport.limits.max_connected_peers",
            u64::from(limits.max_connected_peers),
            1,
            2_048,
        ),
        (
            "transport.limits.max_connections_total",
            u64::from(limits.max_connections_total),
            1,
            4_096,
        ),
        (
            "transport.limits.max_connections_per_peer",
            u64::from(limits.max_connections_per_peer),
            1,
            8,
        ),
        (
            "transport.limits.max_candidates",
            u64::from(limits.max_candidates),
            1,
            16_384,
        ),
        (
            "transport.limits.max_addresses_per_peer",
            u64::from(limits.max_addresses_per_peer),
            1,
            32,
        ),
        (
            "transport.limits.max_subscriptions",
            u64::from(limits.max_subscriptions),
            1,
            1_024,
        ),
        (
            "transport.pre_auth.handshake_timeout",
            u64::from(pre_auth.handshake_timeout_ms),
            5_000,
            30_000,
        ),
        (
            "transport.pre_auth.max_pending_inbound_handshakes",
            u64::from(pre_auth.max_pending_inbound_handshakes),
            1,
            256,
        ),
        (
            "transport.pre_auth.max_pending_per_source_bucket",
            u64::from(pre_auth.max_pending_per_source_bucket),
            1,
            32,
        ),
        (
            "transport.pre_auth.max_attempts_per_source_bucket_per_minute",
            u64::from(pre_auth.max_attempts_per_source_bucket_per_minute),
            1,
            600,
        ),
        (
            "transport.pre_auth.max_attempts_global_per_minute",
            u64::from(pre_auth.max_attempts_global_per_minute),
            1,
            6_000,
        ),
        (
            "transport.pre_auth.ipv6_source_prefix_bits",
            u64::from(pre_auth.ipv6_source_prefix_bits),
            64,
            64,
        ),
        (
            "transport.connection_policy.address_backoff_min",
            u64::from(policy.address_backoff_min_ms),
            1_000,
            60_000,
        ),
        (
            "transport.connection_policy.address_backoff_max",
            u64::from(policy.address_backoff_max_ms),
            30_000,
            1_800_000,
        ),
        (
            "transport.connection_policy.identity_mismatch_quarantine",
            u64::from(policy.identity_mismatch_quarantine_ms),
            60_000,
            86_400_000,
        ),
        (
            "transport.direct.timeout_ms",
            u64::from(direct.timeout_ms),
            1_000,
            60_000,
        ),
        (
            "transport.direct.max_inflight_total",
            u64::from(direct.max_inflight_total),
            1,
            512,
        ),
        (
            "transport.direct.max_inflight_per_peer",
            u64::from(direct.max_inflight_per_peer),
            1,
            32,
        ),
    ];
    rows.extend(rate_rows(
        &[
            "transport.direct.inbound_rate_limit.per_peer_per_minute",
            "transport.direct.inbound_rate_limit.per_peer_burst",
            "transport.direct.inbound_rate_limit.global_per_minute",
            "transport.direct.inbound_rate_limit.global_burst",
        ],
        &direct.inbound_rate_limit,
    ));
    rows.extend(rate_rows(
        &[
            "transport.pubsub.inbound_rate_limit.per_peer_per_minute",
            "transport.pubsub.inbound_rate_limit.per_peer_burst",
            "transport.pubsub.inbound_rate_limit.global_per_minute",
            "transport.pubsub.inbound_rate_limit.global_burst",
        ],
        &pubsub.inbound_rate_limit,
    ));
    for (field, got, lo, hi) in rows {
        if !(lo..=hi).contains(&got) {
            errors.push(ConfigError::OutOfRange {
                field,
                got,
                allowed: (lo, hi),
            });
        }
    }
    let pinned: [(&'static str, bool); 5] = [
        (
            "transport.direct.inbound_rate_limit.enabled",
            direct.inbound_rate_limit.enabled,
        ),
        (
            "transport.pubsub.inbound_rate_limit.enabled",
            pubsub.inbound_rate_limit.enabled,
        ),
        ("transport.pubsub.signed_messages", pubsub.signed_messages),
        (
            "transport.pubsub.strict_validation",
            pubsub.strict_validation,
        ),
        (
            "transport.pubsub.explicit_application_validation",
            pubsub.explicit_application_validation,
        ),
    ];
    for (field, holds) in pinned {
        if !holds {
            errors.push(ConfigError::LiteralViolated { field });
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]
    use super::*;
    use crate::connectivity::TransportConfig;

    fn parse(yaml: &str) -> Result<TransportConfig, serde_norway::Error> {
        serde_norway::from_str(yaml)
    }

    fn errors_of(t: &TransportConfig) -> Vec<ConfigError> {
        let mut errors = Vec::new();
        validate_into(
            &t.listen,
            &t.limits,
            &t.pre_auth,
            &t.connection_policy,
            &t.direct,
            &t.pubsub,
            &mut errors,
        );
        errors
    }

    fn out_of_range_fields(errors: &[ConfigError]) -> Vec<&'static str> {
        errors
            .iter()
            .filter_map(|e| match e {
                ConfigError::OutOfRange { field, .. } => Some(*field),
                _ => None,
            })
            .collect()
    }

    /// An empty `transport` is every schema default, and the defaults are
    /// valid: a profile that states nothing asks for nothing unusual.
    #[test]
    fn the_defaults_are_the_schemas_and_are_valid() {
        let t = parse("{}").expect("an empty block parses");
        assert_eq!(t.backend, TransportBackend::Libp2p);
        assert!(t.listen.addresses.is_empty());
        assert_eq!(t.limits.max_payload_bytes, 49_152);
        assert_eq!(t.limits.max_connections_total, 384);
        assert_eq!(t.pre_auth.handshake_timeout_ms, 10_000);
        assert_eq!(t.pre_auth.ipv6_source_prefix_bits, 64);
        assert_eq!(t.connection_policy.address_backoff_max_ms, 300_000);
        assert_eq!(
            t.connection_policy.identity_mismatch_quarantine_ms,
            1_800_000
        );
        assert_eq!(t.direct.protocol, DirectProtocol::DirectV2);
        assert_eq!(t.direct.inbound_rate_limit.per_peer_per_minute, 120);
        assert!(t.pubsub.signed_messages && t.pubsub.strict_validation);
        assert_eq!(
            t.pubsub.message_id_function,
            MessageIdFunction::SourcePeerSeqnoSha256V1
        );
        assert!(errors_of(&t).is_empty(), "{:?}", errors_of(&t));
    }

    /// Every range row is checked at both edges, named by its path: one
    /// below and one above each bound is refused, the bounds themselves
    /// are not.
    #[test]
    fn every_range_is_checked_at_both_edges_by_name() {
        let (lo, hi) = (
            parse(
                "limits: {max_payload_bytes: 1024, max_connected_peers: 1, max_connections_total: 1, max_connections_per_peer: 1, max_candidates: 1, max_addresses_per_peer: 1, max_subscriptions: 1}
pre_auth: {handshake_timeout: 5s, max_pending_inbound_handshakes: 1, max_pending_per_source_bucket: 1, max_attempts_per_source_bucket_per_minute: 1, max_attempts_global_per_minute: 1}
connection_policy: {address_backoff_min: 1s, address_backoff_max: 30s, identity_mismatch_quarantine: 1m}
direct: {timeout_ms: 1000, max_inflight_total: 1, max_inflight_per_peer: 1, inbound_rate_limit: {per_peer_per_minute: 1, per_peer_burst: 1, global_per_minute: 1, global_burst: 1}}
pubsub: {inbound_rate_limit: {per_peer_per_minute: 1, per_peer_burst: 1, global_per_minute: 1, global_burst: 1}}",
            )
            .expect("parses"),
            parse(
                "limits: {max_payload_bytes: 49152, max_connected_peers: 2048, max_connections_total: 4096, max_connections_per_peer: 8, max_candidates: 16384, max_addresses_per_peer: 32, max_subscriptions: 1024}
pre_auth: {handshake_timeout: 30s, max_pending_inbound_handshakes: 256, max_pending_per_source_bucket: 32, max_attempts_per_source_bucket_per_minute: 600, max_attempts_global_per_minute: 6000}
connection_policy: {address_backoff_min: 1m, address_backoff_max: 30m, identity_mismatch_quarantine: 24h}
direct: {timeout_ms: 60000, max_inflight_total: 512, max_inflight_per_peer: 32, inbound_rate_limit: {per_peer_per_minute: 6000, per_peer_burst: 512, global_per_minute: 60000, global_burst: 2048}}
pubsub: {inbound_rate_limit: {per_peer_per_minute: 6000, per_peer_burst: 512, global_per_minute: 60000, global_burst: 2048}}",
            )
            .expect("parses"),
        );
        assert!(
            errors_of(&lo).is_empty(),
            "lower bounds: {:?}",
            errors_of(&lo)
        );
        assert!(
            errors_of(&hi).is_empty(),
            "upper bounds: {:?}",
            errors_of(&hi)
        );

        let below = parse(
            "limits: {max_payload_bytes: 1023, max_connected_peers: 0, max_connections_total: 0, max_connections_per_peer: 0, max_candidates: 0, max_addresses_per_peer: 0, max_subscriptions: 0}
pre_auth: {handshake_timeout: 4999ms, max_pending_inbound_handshakes: 0, max_pending_per_source_bucket: 0, max_attempts_per_source_bucket_per_minute: 0, max_attempts_global_per_minute: 0, ipv6_source_prefix_bits: 63}
connection_policy: {address_backoff_min: 999ms, address_backoff_max: 29s, identity_mismatch_quarantine: 59s}
direct: {timeout_ms: 999, max_inflight_total: 0, max_inflight_per_peer: 0, inbound_rate_limit: {per_peer_per_minute: 0, per_peer_burst: 0, global_per_minute: 0, global_burst: 0}}
pubsub: {inbound_rate_limit: {per_peer_per_minute: 0, per_peer_burst: 0, global_per_minute: 0, global_burst: 0}}",
        )
        .expect("parses");
        let above = parse(
            "limits: {max_payload_bytes: 49153, max_connected_peers: 2049, max_connections_total: 4097, max_connections_per_peer: 9, max_candidates: 16385, max_addresses_per_peer: 33, max_subscriptions: 1025}
pre_auth: {handshake_timeout: 30001ms, max_pending_inbound_handshakes: 257, max_pending_per_source_bucket: 33, max_attempts_per_source_bucket_per_minute: 601, max_attempts_global_per_minute: 6001, ipv6_source_prefix_bits: 65}
connection_policy: {address_backoff_min: 61s, address_backoff_max: 31m, identity_mismatch_quarantine: 25h}
direct: {timeout_ms: 60001, max_inflight_total: 513, max_inflight_per_peer: 33, inbound_rate_limit: {per_peer_per_minute: 6001, per_peer_burst: 513, global_per_minute: 60001, global_burst: 2049}}
pubsub: {inbound_rate_limit: {per_peer_per_minute: 6001, per_peer_burst: 513, global_per_minute: 60001, global_burst: 2049}}",
        )
        .expect("parses");
        let expected: Vec<&str> = vec![
            "transport.limits.max_payload_bytes",
            "transport.limits.max_connected_peers",
            "transport.limits.max_connections_total",
            "transport.limits.max_connections_per_peer",
            "transport.limits.max_candidates",
            "transport.limits.max_addresses_per_peer",
            "transport.limits.max_subscriptions",
            "transport.pre_auth.handshake_timeout",
            "transport.pre_auth.max_pending_inbound_handshakes",
            "transport.pre_auth.max_pending_per_source_bucket",
            "transport.pre_auth.max_attempts_per_source_bucket_per_minute",
            "transport.pre_auth.max_attempts_global_per_minute",
            "transport.pre_auth.ipv6_source_prefix_bits",
            "transport.connection_policy.address_backoff_min",
            "transport.connection_policy.address_backoff_max",
            "transport.connection_policy.identity_mismatch_quarantine",
            "transport.direct.timeout_ms",
            "transport.direct.max_inflight_total",
            "transport.direct.max_inflight_per_peer",
            "transport.direct.inbound_rate_limit.per_peer_per_minute",
            "transport.direct.inbound_rate_limit.per_peer_burst",
            "transport.direct.inbound_rate_limit.global_per_minute",
            "transport.direct.inbound_rate_limit.global_burst",
            "transport.pubsub.inbound_rate_limit.per_peer_per_minute",
            "transport.pubsub.inbound_rate_limit.per_peer_burst",
            "transport.pubsub.inbound_rate_limit.global_per_minute",
            "transport.pubsub.inbound_rate_limit.global_burst",
        ];
        assert_eq!(out_of_range_fields(&errors_of(&below)), expected);
        assert_eq!(out_of_range_fields(&errors_of(&above)), expected);
    }

    /// A `literal[true]` flag set false is refused by name; a string
    /// literal spelled otherwise does not parse at all.
    #[test]
    fn literals_are_enforced() {
        let t = parse(
            "direct: {inbound_rate_limit: {enabled: false}}
pubsub: {signed_messages: false, strict_validation: false, explicit_application_validation: false, inbound_rate_limit: {enabled: false}}",
        )
        .expect("parses");
        let fields: Vec<&str> = errors_of(&t)
            .iter()
            .filter_map(|e| match e {
                ConfigError::LiteralViolated { field } => Some(*field),
                _ => None,
            })
            .collect();
        assert_eq!(
            fields,
            [
                "transport.direct.inbound_rate_limit.enabled",
                "transport.pubsub.inbound_rate_limit.enabled",
                "transport.pubsub.signed_messages",
                "transport.pubsub.strict_validation",
                "transport.pubsub.explicit_application_validation",
            ]
        );
        for refused in [
            "backend: quic",
            "direct: {protocol: /interweave/direct/1.0.0}",
            "pubsub: {backend: floodsub}",
            "pubsub: {message_id_function: envelope-id}",
        ] {
            assert!(parse(refused).is_err(), "{refused} parsed");
        }
        for accepted in [
            "backend: libp2p",
            "direct: {protocol: /interweave/direct/2.0.0}",
            "pubsub: {backend: gossipsub, message_id_function: source-peer-seqno-sha256-v1}",
        ] {
            assert!(parse(accepted).is_ok(), "{accepted} refused");
        }
    }

    /// An unknown key is refused at every level, not ignored.
    #[test]
    fn unknown_keys_are_refused_at_every_level() {
        for doc in [
            "listen: {addresses: [], port: 1}",
            "limits: {max_peers: 1}",
            "pre_auth: {timeout: 5s}",
            "connection_policy: {backoff: 5s}",
            "direct: {rate: 1}",
            "direct: {inbound_rate_limit: {burst: 1}}",
            "pubsub: {flood: true}",
            "wire: {}",
        ] {
            assert!(parse(doc).is_err(), "{doc} parsed");
        }
    }

    #[test]
    fn listen_addresses_are_bounded() {
        let at = format!(
            "listen: {{addresses: [{}]}}",
            vec!["\"/ip4/0.0.0.0/tcp/0\""; MAX_LISTEN_ADDRESSES].join(", ")
        );
        assert!(errors_of(&parse(&at).expect("parses")).is_empty());
        let over = format!(
            "listen: {{addresses: [{}]}}",
            vec!["\"/ip4/0.0.0.0/tcp/0\""; MAX_LISTEN_ADDRESSES + 1].join(", ")
        );
        assert_eq!(
            out_of_range_fields(&errors_of(&parse(&over).expect("parses"))),
            ["transport.listen.addresses"]
        );
    }
}
