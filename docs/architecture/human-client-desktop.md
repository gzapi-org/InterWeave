# Human client — desktop architecture

Status: architecture/design only.

## Deployment model

Windows, macOS, and Linux use the existing external profile daemon and IPC v2 boundary:

```text
+---------------- Rust human-desktop ----------------+
| Slint UI                                             |
| human-core / human-store                             |
|                                                     |
| data IPC client -----------+                         |
| admin IPC client ----------|--- explicit settings   |
+----------------------------|-------------------------+
                             |
             +---------------+---------------+
             |                               |
     <profile>.sock                 <profile>-admin.sock
             |                               |
             +---------------+---------------+
                             v
                    Rust transport-daemon
                             |
                           libp2p
```

The human executable never links to `transport-libp2p` and never loads the profile private key.

## Startup

1. resolve selected profile and runtime paths;
2. open application database;
3. connect to data socket;
4. if daemon is unavailable, the launcher may start the configured daemon executable/service and retry with bounded backoff;
5. negotiate IPC v2 and keepalive;
6. claim configured `human` EndpointId;
7. load caller subscriptions/status/connectivity;
8. begin consuming the bounded event stream;
9. open admin socket only when a settings action requires it.

Closing the UI releases its EndpointId lease but does **not** imply daemon shutdown. This allows Claude or other endpoints to remain online under the same profile.

## Desktop administration

The UI has separate code paths:

- data plane: ordinary messaging/status;
- settings/admin: explicit local trust, endpoint, bootstrap/infrastructure, diagnostics operations over admin socket;
- identity backup/restore: still an offline `transportctl identity ...` workflow and never proxied through daemon IPC.

The desktop app may guide the user to recovery tooling, but it does not receive the 24-word phrase through IPC.

## Packaging

Reference packaging consists of:

- `human-desktop` Rust/Slint application;
- `transport-daemon` Rust binary/service;
- `transportctl` Rust administrative/offline identity tool;
- shared profile/config directories;
- platform service/autostart integration.

Autostart of the daemon is user/operator policy. The human UI can be closed while daemon connectivity remains active.

## Local persistence

Use a versioned SQLite application database accessed by Rust. Message content follows the frozen [`clients/human/RETENTION.md`](../../clients/human/RETENTION.md) state machine rather than a conventional permanent history:

- `pending_outbound` survives until transport-terminal success/cancel;
- `unread_inbound` survives until local read state;
- `kept_inbound` exists only after the receiver explicitly chooses Keep after reading;
- transport-terminal outbound and read-unkept inbound are RAM-only current-session content and evaporate across restart.

Persist contacts, route preferences, the three permitted retention sets, and separately allowed UI preferences. No database row is interpreted as proof of remote human read/processing.

Database corruption must not damage transport identity/config: human application storage and profile identity/config remain separate files/namespaces.

## Desktop security

- OS owner ACLs protect profile sockets; admin socket may use stricter ACL/group/service-account policy.
- normal UI never obtains daemon private key bytes.
- pending/unread/kept message content contains sensitive plaintext and should follow platform storage protections; optional application-database encryption is a separate application hardening choice. Deleted read-unkept/terminal content must not be intentionally copied into shadow history tables/search indexes/logs.
- passphrase-encrypted transport identity storage remains ADR-0038/v2.x rather than being invented inside the human app.

## Testing

Required desktop integration tests include daemon absent/start, daemon already shared with Claude, exact endpoint lease conflict, admin/data socket separation, slow UI queue, UI crash/reconnect, daemon restart, human+Claude different endpoint routing, database unavailable/corrupt without identity corruption, retention-state restart cases from ADR-0044, and network path changes direct<->relay without conversation-route mutation.
