// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton

//! The `runtime` block: which deployment binding this profile runs under.
//!
//! `config.schema.yaml` is normative for the shape; this module is the
//! first code to read it (plan §15's Stage 12 decisions, 2026-09-27:
//! composition needs `runtime.deployment` to choose what it builds, and
//! modelling the block is what lets the schema's `# Runtime cross-field
//! validation` be enforced at all).
//!
//! # Which of the six runtime rules are checked here
//!
//! The schema lists six, and a rule is checked only where the model
//! holds both its sides:
//!
//! 1. and 2. `daemon-ipc` runs the IPC boundary and `embedded-android`
//!    does not (`ipc.enabled`) -- checked since Stage 13 modelled `ipc`
//!    ([`RuntimeConfig::validate_into`]).
//! 3. `embedded-android` names an enabled configured endpoint -- checked
//!    ([`RuntimeConfig::validate_into`]).
//! 4. `embedded-android` runs no AutoNAT or relay server and Kademlia
//!    only as a client -- checked.
//! 5. `stay-reachable` means `foreground_service_type = remoteMessaging`
//!    -- held by the TYPE: the field is `literal[remoteMessaging]`, so no
//!    other value parses whatever the availability mode.
//! 6. `stay-reachable` with `user-presence` derives a diagnostic, not a
//!    refusal -- [`AndroidRuntimeConfig::background_restart_requires_user_authentication`].
//!
//! And a seventh the schema gained after the six (architect-cto's
//! ruling of 2026-10-09, relay seq 33736, §20 step 5): `embedded-android`
//! listens on WILDCARD addresses only (`/ip4/0.0.0.0`, `/ip6/::`). A
//! listener on one address dies with it and nothing issues it again,
//! and Android names no address that stays; a wildcard listener follows
//! the interfaces as they come and go. Checked
//! ([`RuntimeConfig::validate_into`]); a daemon may name a specific one.

use interweave_transport_api::EndpointId;
use serde::{Deserialize, Serialize};

use crate::{ConfigError, DiscoveryProviderType, EndpointsConfig};

/// The Android endpoint when the profile names none (`EndpointId? = human`).
pub const DEFAULT_ANDROID_ENDPOINT: &str = "human";

/// The `runtime` block.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeConfig {
    /// The deployment binding (`daemon-ipc` by default).
    #[serde(default)]
    pub deployment: Deployment,
    /// Read only when `deployment` is `embedded-android`.
    #[serde(default)]
    pub android: AndroidRuntimeConfig,
}

/// How the transport runtime is hosted.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Deployment {
    /// A daemon serving local clients over IPC.
    #[default]
    DaemonIpc,
    /// In-process in the Android app's foreground service.
    EmbeddedAndroid,
}

/// `runtime.android`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AndroidRuntimeConfig {
    /// The endpoint the foreground service leases; absent is
    /// [`DEFAULT_ANDROID_ENDPOINT`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<EndpointId>,
    /// Whether the service keeps the node reachable in the background.
    #[serde(default)]
    pub availability_mode: AvailabilityMode,
    /// `literal[remoteMessaging]`.
    #[serde(default)]
    pub foreground_service_type: ForegroundServiceType,
    /// `literal[android-keystore-aesgcm-wrap]`.
    #[serde(default)]
    pub key_storage: KeyStorage,
    /// Whether unlocking the key needs the user present.
    #[serde(default)]
    pub key_unlock_policy: KeyUnlockPolicy,
}

impl AndroidRuntimeConfig {
    /// The endpoint the service leases, the default applied.
    #[must_use]
    pub fn endpoint(&self) -> Option<EndpointId> {
        self.endpoint
            .clone()
            .or_else(|| EndpointId::parse(DEFAULT_ANDROID_ENDPOINT).ok())
    }

    /// The schema's sixth runtime rule: a node that stays reachable but
    /// whose key needs the user present cannot come back on its own
    /// after a restart. A derived status, not a refusal -- the
    /// combination is legal.
    #[must_use]
    pub fn background_restart_requires_user_authentication(&self) -> bool {
        self.availability_mode == AvailabilityMode::StayReachable
            && self.key_unlock_policy == KeyUnlockPolicy::UserPresence
    }
}

/// `runtime.android.availability_mode`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AvailabilityMode {
    /// Reachable while the app is in the foreground.
    #[default]
    ForegroundOnly,
    /// Kept reachable by the foreground service.
    StayReachable,
}

/// `runtime.android.foreground_service_type`: one value, so a profile may
/// state it and may state nothing else.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ForegroundServiceType {
    /// Android's `remoteMessaging` foreground-service type.
    #[default]
    #[serde(rename = "remoteMessaging")]
    RemoteMessaging,
}

/// `runtime.android.key_storage`: one value, as above.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum KeyStorage {
    /// The identity key wrapped by an Android Keystore AES-GCM key.
    #[default]
    #[serde(rename = "android-keystore-aesgcm-wrap")]
    AndroidKeystoreAesgcmWrap,
}

/// `runtime.android.key_unlock_policy`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum KeyUnlockPolicy {
    /// Usable after a restart without the user.
    #[default]
    BackgroundCompatible,
    /// Needs local user authentication to unlock.
    UserPresence,
}

/// What the runtime rules read from the rest of the document.
pub(crate) struct RuntimeContext<'a> {
    pub endpoints: &'a EndpointsConfig,
    pub autonat_server_enabled: bool,
    pub relay_server_enabled: bool,
    /// Each enabled Kademlia entry's `mode`, as written (`None` is the
    /// schema default, `client`).
    pub enabled_kademlia_modes: Vec<Option<&'a str>>,
    /// `ipc.enabled`.
    pub ipc_enabled: bool,
    /// `transport.listen.addresses`, as written.
    pub listen_addresses: &'a [String],
}

impl RuntimeConfig {
    /// Rules 1 to 4 and the seventh, the wildcard listeners (see the
    /// module note for all seven).
    pub(crate) fn validate_into(
        &self,
        context: &RuntimeContext<'_>,
        errors: &mut Vec<ConfigError>,
    ) {
        let android = self.deployment == Deployment::EmbeddedAndroid;
        // Rules 1 and 2: the daemon serves its clients over IPC; the
        // embedded runtime has no IPC boundary to open.
        if context.ipc_enabled == android {
            errors.push(ConfigError::IpcContradictsDeployment {
                deployment: if android {
                    "embedded-android"
                } else {
                    "daemon-ipc"
                },
                ipc_enabled: context.ipc_enabled,
            });
        }
        if !android {
            return;
        }
        let endpoint = self.android.endpoint();
        let enabled = endpoint.as_ref().is_some_and(|id| {
            context
                .endpoints
                .entries
                .iter()
                .any(|e| &e.id == id && e.enabled)
        });
        if !enabled {
            errors.push(ConfigError::AndroidEndpointNotEnabled {
                endpoint: endpoint.map_or_else(
                    || DEFAULT_ANDROID_ENDPOINT.to_owned(),
                    |id| id.as_str().to_owned(),
                ),
            });
        }
        for (field, on) in [
            (
                "transport.connectivity.autonat.server.enabled",
                context.autonat_server_enabled,
            ),
            (
                "transport.connectivity.relay.server.enabled",
                context.relay_server_enabled,
            ),
        ] {
            if on {
                errors.push(ConfigError::AndroidServesInfrastructure { field });
            }
        }
        if context
            .enabled_kademlia_modes
            .iter()
            .any(|mode| mode.is_some_and(|m| m != "client"))
        {
            errors.push(ConfigError::AndroidServesInfrastructure {
                field: "discovery.providers[kademlia].config.mode",
            });
        }
        for address in context.listen_addresses {
            if !is_wildcard_listener(address) {
                errors.push(ConfigError::AndroidListenerNotWildcard {
                    address: address.clone(),
                });
            }
        }
    }
}

/// Whether `address` binds every interface of its family: its first
/// component is `/ip4/0.0.0.0` or `/ip6/::`. Read as text, since this
/// crate names no backend type; anything else -- a specific IP, a name,
/// a string that is no multiaddr -- is not a wildcard.
fn is_wildcard_listener(address: &str) -> bool {
    let mut parts = address.split('/');
    match (parts.next(), parts.next(), parts.next()) {
        (Some(""), Some(family @ ("ip4" | "ip6")), Some(host)) => host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_unspecified() && (family == "ip4") == ip.is_ipv4()),
        _ => false,
    }
}

/// The context [`RuntimeConfig::validate_into`] reads, gathered from a
/// whole profile.
pub(crate) fn context(profile: &crate::ProfileConfig) -> RuntimeContext<'_> {
    let connectivity = &profile.transport.connectivity;
    RuntimeContext {
        endpoints: &profile.endpoints,
        autonat_server_enabled: connectivity.autonat.server.enabled,
        relay_server_enabled: connectivity.relay.server.enabled,
        enabled_kademlia_modes: profile
            .discovery
            .providers
            .iter()
            .filter(|p| p.enabled && p.provider_type == DiscoveryProviderType::Kademlia)
            .map(|p| p.config.mode.as_deref())
            .collect(),
        ipc_enabled: profile.ipc.enabled,
        listen_addresses: &profile.transport.listen.addresses,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]
    use super::{
        AndroidRuntimeConfig, AvailabilityMode, DEFAULT_ANDROID_ENDPOINT, KeyUnlockPolicy,
        RuntimeConfig,
    };

    #[test]
    fn the_default_endpoint_is_a_legal_endpoint_id() {
        assert_eq!(
            AndroidRuntimeConfig::default()
                .endpoint()
                .expect("parses")
                .as_str(),
            DEFAULT_ANDROID_ENDPOINT
        );
    }

    #[test]
    fn the_literals_refuse_any_other_value() {
        for (key, value) in [
            ("foreground_service_type", "dataSync"),
            ("key_storage", "plaintext"),
        ] {
            let doc =
                format!(r#"{{"deployment":"embedded-android","android":{{"{key}":"{value}"}}}}"#);
            assert!(
                serde_json::from_str::<RuntimeConfig>(&doc).is_err(),
                "{key}={value} parsed"
            );
        }
        let stated = r#"{"android":{"foreground_service_type":"remoteMessaging",
            "key_storage":"android-keystore-aesgcm-wrap"}}"#;
        assert!(
            serde_json::from_str::<RuntimeConfig>(stated).is_ok(),
            "the control: the literal values themselves parse"
        );
    }

    #[test]
    fn only_stay_reachable_with_user_presence_needs_the_user_after_restart() {
        for (mode, policy, expected) in [
            (
                AvailabilityMode::StayReachable,
                KeyUnlockPolicy::UserPresence,
                true,
            ),
            (
                AvailabilityMode::StayReachable,
                KeyUnlockPolicy::BackgroundCompatible,
                false,
            ),
            (
                AvailabilityMode::ForegroundOnly,
                KeyUnlockPolicy::UserPresence,
                false,
            ),
            (
                AvailabilityMode::ForegroundOnly,
                KeyUnlockPolicy::BackgroundCompatible,
                false,
            ),
        ] {
            let android = AndroidRuntimeConfig {
                availability_mode: mode,
                key_unlock_policy: policy,
                ..AndroidRuntimeConfig::default()
            };
            assert_eq!(
                android.background_restart_requires_user_authentication(),
                expected,
                "{mode:?} + {policy:?}"
            );
        }
    }
}
