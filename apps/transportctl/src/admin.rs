// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The admin commands: one method against the profile's running daemon,
//! over its admin socket (plan §16 (10)).

use std::fmt::Write as _;

use interweave_ipc_client::{IpcAdmin, IpcBinding, SocketPaths};
use interweave_local_client_api::{AdminBinding as _, AdminCapability, AdminPort as _};
use interweave_profile_config::{ProfileLock, ProfilePaths, XdgRoots};
use interweave_transport_api::TransportError;

use crate::Failure;
use crate::cli::Admin;

/// The client kind this tool names itself on the admin socket. Not an
/// authority: on the data socket it buys nothing (ADR-0037).
const CLIENT_KIND: &str = "transportctl";

/// Run `action` against `profile`'s daemon; what to print on success.
///
/// # Errors
/// [`Failure::Unreachable`] when no daemon answers on the admin socket,
/// told apart by the profile lock; [`Failure::Refused`] with the error
/// code when the daemon refuses.
pub(crate) async fn run(profile: &str, action: Admin, json: bool) -> Result<String, Failure> {
    let refused = |what: &str, e: &dyn std::fmt::Display| Failure::Refused(format!("{what}: {e}"));
    let roots = XdgRoots::from_env().map_err(|e| refused("the XDG directories", &e))?;
    let paths = ProfilePaths::resolve(profile, &roots).map_err(crate::profile_refusal)?;
    let sockets = SocketPaths {
        data: paths
            .data_socket()
            .map_err(|e| refused("the data socket", &e))?,
        admin: paths
            .admin_socket()
            .map_err(|e| refused("the admin socket", &e))?,
    };
    let binding = IpcBinding::new(sockets.clone(), CLIENT_KIND);
    let capability = match action {
        Admin::Status => AdminCapability::Status,
        Admin::Shutdown(_) => AdminCapability::Shutdown,
        Admin::TrustList | Admin::SetTrust(..) => AdminCapability::Trust,
        _ => AdminCapability::Endpoints,
    };
    let port = match binding.admin([capability].into()).await {
        Ok(port) => port,
        Err(TransportError::BackendUnavailable) => {
            return Err(unreachable(&paths, &sockets));
        }
        Err(e) => return Err(code(e)),
    };
    // A 2.0 daemon is not asked for a 2.1 capability, so the port comes
    // back without it: said here, rather than as the refusal a request
    // would draw.
    if !port.port().holds(capability) {
        return Err(Failure::Refused(format!(
            "the daemon does not grant {capability:?} (it is IPC 2.1's: is the daemon older?)"
        )));
    }
    call(&port, action, json).await.map_err(code)
}

/// Tell "no daemon" from "a daemon whose socket is gone" by the lock: a
/// `try_lock` released at once, which the daemon's own one-second retry
/// absorbs, so a probe cannot fail a starting daemon (plan §16 (10)).
fn unreachable(paths: &ProfilePaths, sockets: &SocketPaths) -> Failure {
    match ProfileLock::is_held(paths) {
        Ok(false) => Failure::Unreachable(format!(
            "no daemon is running for profile {:?}",
            paths.profile()
        )),
        Ok(true) => Failure::Unreachable(format!(
            "a daemon holds profile {:?}'s lock, but its admin socket {} does not answer",
            paths.profile(),
            sockets.admin.display()
        )),
        Err(e) => Failure::Unreachable(format!(
            "the admin socket {} does not answer, and the lock could not be probed: {e}",
            sockets.admin.display()
        )),
    }
}

/// A daemon's refusal: its error code, as the wire names it.
fn code(e: TransportError) -> Failure {
    Failure::Refused(format!("refused: {e:?}"))
}

/// A result object as `--json` prints it: the method's own shape, so the
/// output validates against its schema (plan §16 (11)).
fn to_json<T: serde::Serialize>(result: &T) -> String {
    // A result that came off the wire as JSON goes back to it.
    serde_json::to_string_pretty(result).unwrap_or_default() + "\n"
}

async fn call(port: &IpcAdmin, action: Admin, json: bool) -> Result<String, TransportError> {
    Ok(match action {
        Admin::Status => {
            let status = port.status_result().await?;
            if json {
                to_json(&status)
            } else {
                let mut out = String::new();
                let _ = writeln!(out, "peer         {}", status.peer.as_str());
                let _ = writeln!(out, "health       {:?}", status.health);
                let _ = writeln!(
                    out,
                    "connections  {} data, {} admin",
                    status.ipc.data_connections, status.ipc.admin_connections
                );
                let _ = writeln!(out, "leases       {}", status.ipc.active_leases);
                let c = &status.connectivity;
                let _ = writeln!(
                    out,
                    "inbound      direct {:?}, relay {:?} ({}/{} reservations)",
                    c.direct_inbound,
                    c.relay_inbound,
                    c.active_relay_reservations,
                    c.target_relay_reservations
                );
                out
            }
        }
        Admin::EndpointsList => {
            let list = port.endpoints_result().await?;
            if json {
                to_json(&list)
            } else {
                let mut out = String::new();
                for row in &list.endpoints {
                    let _ = writeln!(
                        out,
                        "{:<24} {}{}{}",
                        row.id.as_str(),
                        if row.enabled { "enabled" } else { "disabled" },
                        if row.default { ", default" } else { "" },
                        row.lease.as_ref().map_or_else(String::new, |lease| format!(
                            ", leased by {}",
                            lease.client_kind
                        )),
                    );
                }
                out
            }
        }
        Admin::Revoke(endpoint) => {
            port.revoke_endpoint(endpoint.clone()).await?;
            format!("revoked {}\n", endpoint.as_str())
        }
        Admin::SetEnabled(endpoint, enabled) => {
            let revoked = port.set_endpoint_enabled(endpoint.clone(), enabled).await?;
            format!(
                "{} {}{}\n",
                if enabled { "enabled" } else { "disabled" },
                endpoint.as_str(),
                if revoked.is_some() {
                    ", its lease revoked"
                } else {
                    ""
                }
            )
        }
        Admin::SetDefault(endpoint) => {
            port.set_default_endpoint(endpoint.clone()).await?;
            match endpoint {
                Some(endpoint) => format!("default endpoint {}\n", endpoint.as_str()),
                None => "no default endpoint\n".to_owned(),
            }
        }
        Admin::Shutdown(grace) => {
            // No --grace: none sent, the daemon's default.
            port.request_shutdown(grace).await?;
            "shutdown requested\n".to_owned()
        }
        Admin::TrustList => {
            let pages = port.trust_pages().await?;
            let mut out = String::new();
            if json {
                // ONE `ipc/trust-list` PAGE PER LINE, as the daemon sent
                // each: the method answers in pages, so a single document
                // would be a shape no schema states.
                for page in &pages {
                    out += &serde_json::to_string(page).unwrap_or_default();
                    out.push('\n');
                }
            } else {
                if let Some(local) = pages.first().and_then(|page| page.local_peer.as_ref()) {
                    let _ = writeln!(out, "local    {}", local.as_str());
                }
                for row in pages.iter().flat_map(|page| &page.allowed) {
                    let _ = writeln!(out, "allowed  {}", row.peer.as_str());
                }
                out += "every other peer is denied\n";
            }
            out
        }
        Admin::SetTrust(peer, allowed) => {
            port.set_trust(peer.clone(), allowed).await?;
            format!(
                "{} {}\n",
                if allowed { "allowed" } else { "revoked" },
                peer.as_str()
            )
        }
    })
}
