// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton

//! The schema's `# Runtime cross-field validation`, the rules the model
//! makes checkable (`runtime.rs`'s module note names all seven): an
//! embedded-android profile leases an enabled configured endpoint, runs
//! no infrastructure server, and listens on wildcard addresses only. Each refusal beside its control -- the
//! same document as a daemon, or with the endpoint enabled -- so a
//! refusal is the rule's and not the document's.

#![allow(clippy::expect_used)]

use interweave_profile_config::{ConfigError, ProfileConfig};

fn profile(deployment: &str, endpoint_enabled: bool, extra: &str) -> ProfileConfig {
    let doc = format!(
        "schema_version: 2
runtime:
  deployment: {deployment}
trust:
  policy: static-allowlist
  allowed_peers: []
endpoints:
  entries:
    - id: human
      enabled: {endpoint_enabled}
      advertise: false
{extra}"
    );
    serde_norway::from_str(&doc).expect("the document parses")
}

fn android_errors(profile: &ProfileConfig) -> Vec<ConfigError> {
    profile
        .validate()
        .into_iter()
        .filter(|e| {
            matches!(
                e,
                ConfigError::AndroidEndpointNotEnabled { .. }
                    | ConfigError::AndroidServesInfrastructure { .. }
                    | ConfigError::AndroidListenerNotWildcard { .. }
            )
        })
        .collect()
}

#[test]
fn an_android_profile_with_its_endpoint_enabled_is_accepted() {
    assert!(android_errors(&profile("embedded-android", true, "")).is_empty());
}

#[test]
fn an_android_profile_must_lease_an_enabled_configured_endpoint() {
    assert_eq!(
        android_errors(&profile("embedded-android", false, "")),
        vec![ConfigError::AndroidEndpointNotEnabled {
            endpoint: "human".to_owned()
        }],
        "the default endpoint, disabled"
    );
    let mut named = profile("embedded-android", true, "");
    named.runtime.android.endpoint =
        Some(interweave_transport_api::EndpointId::parse("pager").expect("valid"));
    assert_eq!(
        android_errors(&named),
        vec![ConfigError::AndroidEndpointNotEnabled {
            endpoint: "pager".to_owned()
        }],
        "an endpoint that is not configured at all"
    );
    assert!(
        android_errors(&profile("daemon-ipc", false, "")).is_empty(),
        "the control: a daemon leases nothing at start"
    );
}

#[test]
fn an_android_profile_runs_no_infrastructure_server() {
    for (extra, field) in [
        (
            "transport:\n  connectivity:\n    relay:\n      server:\n        enabled: true\n",
            "transport.connectivity.relay.server.enabled",
        ),
        (
            "transport:\n  connectivity:\n    autonat:\n      server:\n        enabled: true\n",
            "transport.connectivity.autonat.server.enabled",
        ),
        (
            "discovery:\n  providers:\n    - type: kademlia\n      enabled: true\n      priority: 40\n      config:\n        mode: server\n        network_id: interweave-test\n",
            "discovery.providers[kademlia].config.mode",
        ),
    ] {
        assert_eq!(
            android_errors(&profile("embedded-android", true, extra)),
            vec![ConfigError::AndroidServesInfrastructure { field }],
            "{field}"
        );
        assert!(
            android_errors(&profile("daemon-ipc", true, extra)).is_empty(),
            "the control: a daemon may serve {field}"
        );
    }
}

#[test]
fn an_android_profile_listens_on_wildcard_addresses_only() {
    let listening = |addresses: &[&str]| {
        let quoted: Vec<String> = addresses.iter().map(|a| format!("\"{a}\"")).collect();
        format!(
            "transport:\n  listen:\n    addresses: [{}]\n",
            quoted.join(", ")
        )
    };
    let wildcards = listening(&[
        "/ip4/0.0.0.0/tcp/0",
        "/ip6/::/tcp/4001",
        "/ip4/0.0.0.0/udp/0/quic-v1",
    ]);
    assert!(
        android_errors(&profile("embedded-android", true, &wildcards)).is_empty(),
        "the control: every wildcard is accepted"
    );
    for refused in [
        "/ip4/127.0.0.1/tcp/0",
        "/ip4/192.168.1.5/tcp/4001",
        "/ip6/::1/tcp/0",
        "/ip6/0.0.0.0/tcp/0",
        "/ip4/::/tcp/0",
        "/dns4/localhost/tcp/0",
        "ip4/0.0.0.0/tcp/0",
    ] {
        let extra = listening(&["/ip4/0.0.0.0/tcp/0", refused]);
        assert_eq!(
            android_errors(&profile("embedded-android", true, &extra)),
            vec![ConfigError::AndroidListenerNotWildcard {
                address: refused.to_owned()
            }],
            "{refused}"
        );
        assert!(
            !android_errors(&profile("daemon-ipc", true, &extra))
                .iter()
                .any(|e| matches!(e, ConfigError::AndroidListenerNotWildcard { .. })),
            "the control: a daemon may listen on {refused}"
        );
    }
}

#[test]
fn a_disabled_kademlia_entry_in_server_mode_runs_nothing_and_is_not_refused() {
    let extra = "discovery:\n  providers:\n    - type: kademlia\n      enabled: false\n      priority: 40\n      config:\n        mode: server\n        network_id: interweave-test\n";
    assert!(android_errors(&profile("embedded-android", true, extra)).is_empty());
}

#[test]
fn an_unknown_deployment_is_refused_at_parse() {
    let doc = "schema_version: 2
runtime:
  deployment: bare-metal
trust:
  policy: static-allowlist
  allowed_peers: []
endpoints:
  entries: []
";
    assert!(serde_norway::from_str::<ProfileConfig>(doc).is_err());
}

fn ipc_errors(profile: &ProfileConfig) -> Vec<ConfigError> {
    profile
        .validate()
        .into_iter()
        .filter(|e| matches!(e, ConfigError::IpcContradictsDeployment { .. }))
        .collect()
}

/// Rules 1 and 2: a daemon runs its IPC boundary and an embedded Android
/// runtime does not -- each refusal beside the deployment it suits.
#[test]
fn ipc_enabled_follows_the_deployment() {
    assert!(
        ipc_errors(&profile("daemon-ipc", true, "")).is_empty(),
        "the default"
    );
    assert!(
        ipc_errors(&profile(
            "embedded-android",
            true,
            "ipc:\n  enabled: false\n"
        ))
        .is_empty(),
        "android without IPC"
    );
    assert_eq!(
        ipc_errors(&profile("daemon-ipc", true, "ipc:\n  enabled: false\n")),
        vec![ConfigError::IpcContradictsDeployment {
            deployment: "daemon-ipc",
            ipc_enabled: false,
        }]
    );
    assert_eq!(
        ipc_errors(&profile("embedded-android", true, "")),
        vec![ConfigError::IpcContradictsDeployment {
            deployment: "embedded-android",
            ipc_enabled: true,
        }],
        "the default ipc.enabled is a daemon's"
    );
}
