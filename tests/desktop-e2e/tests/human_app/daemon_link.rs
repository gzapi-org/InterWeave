// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The client's link to its daemon: it outlives a daemon restart, taking
//! the lease again and receiving again; and its two connections keep the
//! split IPC authority (ADR-0040) -- no `admin.*` method on the data
//! socket, and on the admin socket only `admin.status`, the facade's
//! read-only status connection, never a lease or a send.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{UnixListener, UnixStream};

use crate::common::human;
use crate::harness::{self as app, human_lease, until_lease};
use crate::world::{self, Peer, envelope, ids, two_daemons, until_rows};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_client_takes_its_lease_again_after_its_daemon_restarts_and_receives() {
    let mut world = two_daemons().await;
    let mut client = app::start(&world.a);
    until_lease(&world.a.binding(), true, &client).await;

    assert!(
        world.a_daemon.terminate().await.success(),
        "{}",
        world.a_daemon.log()
    );
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert!(
        client.running(),
        "the client outlives its daemon: {}",
        client.log()
    );

    world.a_daemon = world.a.start(&[]);
    world.a_daemon.serving(&world.a).await;
    until_lease(&world.a.binding(), true, &client).await;

    let mut peer = Peer::new(&world.b);
    peer.until("B ready", || world.logs(), Peer::is_ready).await;
    let message = envelope(51, "after the daemon came back");
    peer.deliver(&world.a_peer, Some(human()), &message, || world.logs())
        .await;
    until_rows(&world.a, "unread_inbound", 1, || client.log()).await;
    assert_eq!(
        ids(&world.a, "unread_inbound"),
        [message.app_message_id.clone()].into()
    );
    assert!(client.terminate().success(), "{}", client.log());
}

/// One accepted connection: the connecting process, and every byte it
/// sent.
#[derive(Debug, Default)]
struct Seen {
    pid: Option<i32>,
    sent: Vec<u8>,
}

/// A recording proxy standing at a daemon socket's path: the daemon's
/// socket moves aside, and every connection made to the path is passed
/// through to it, recording who connected and what they sent.
struct Tap {
    seen: Arc<Mutex<Vec<Seen>>>,
}

impl Tap {
    fn install(socket: &Path) -> Self {
        let real = PathBuf::from(format!("{}.real", socket.display()));
        std::fs::rename(socket, &real).expect("the daemon's socket moves aside");
        let listener = UnixListener::bind(socket).expect("the tap listens at the path");
        let seen = Arc::new(Mutex::new(Vec::<Seen>::new()));
        let record = Arc::clone(&seen);
        tokio::spawn(async move {
            while let Ok((mut from, _)) = listener.accept().await {
                let pid = from.peer_cred().ok().and_then(|c| c.pid());
                let index = {
                    let mut seen = record.lock().expect("the record");
                    seen.push(Seen {
                        pid,
                        sent: Vec::new(),
                    });
                    seen.len() - 1
                };
                let record = Arc::clone(&record);
                let real = real.clone();
                tokio::spawn(async move {
                    let Ok(mut to) = UnixStream::connect(&real).await else {
                        return;
                    };
                    let (mut from_read, mut from_write) = from.split();
                    let (mut to_read, mut to_write) = to.split();
                    let upstream = async {
                        let mut buffer = [0_u8; 8192];
                        loop {
                            let n = match from_read.read(&mut buffer).await {
                                Ok(0) | Err(_) => break,
                                Ok(n) => n,
                            };
                            record.lock().expect("the record")[index]
                                .sent
                                .extend_from_slice(&buffer[..n]);
                            if to_write.write_all(&buffer[..n]).await.is_err() {
                                break;
                            }
                        }
                        let _ = to_write.shutdown().await;
                    };
                    let downstream = async {
                        let _ = tokio::io::copy(&mut to_read, &mut from_write).await;
                        let _ = from_write.shutdown().await;
                    };
                    tokio::join!(upstream, downstream);
                });
            }
        });
        Self { seen }
    }

    /// What `pid`'s connections sent, each connection's bytes apart.
    fn from(&self, pid: i32) -> Vec<Vec<u8>> {
        self.seen
            .lock()
            .expect("the record")
            .iter()
            .filter(|s| s.pid == Some(pid))
            .map(|s| s.sent.clone())
            .collect()
    }
}

/// What one connection asked for: the capabilities its `hello`
/// requested, and every request method it sent. The IPC frame is a
/// four-byte big-endian length and that many bytes of JSON.
#[derive(Debug, Default)]
struct Asked {
    capabilities: BTreeSet<String>,
    endpoint: Option<String>,
    methods: BTreeSet<String>,
}

fn asked(connections: &[Vec<u8>]) -> Asked {
    let mut asked = Asked::default();
    for mut rest in connections.iter().map(Vec::as_slice) {
        while rest.len() >= 4 {
            let len = usize::try_from(u32::from_be_bytes(rest[..4].try_into().expect("four")))
                .expect("a length");
            let Some(frame) = rest.get(4..4 + len) else {
                break;
            };
            rest = &rest[4 + len..];
            let frame: serde_json::Value = serde_json::from_slice(frame).expect("a JSON frame");
            match frame["type"].as_str() {
                Some("hello") => {
                    asked.capabilities.extend(
                        frame["requested_capabilities"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter_map(|c| c.as_str().map(str::to_owned)),
                    );
                    if let Some(id) = frame["endpoint"]["id"].as_str() {
                        asked.endpoint = Some(id.to_owned());
                    }
                }
                Some("request") => {
                    if let Some(method) = frame["method"].as_str() {
                        asked.methods.insert(method.to_owned());
                    }
                }
                _ => {}
            }
        }
    }
    asked
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_client_keeps_data_and_admin_apart_on_their_own_sockets() {
    let world = two_daemons().await;
    // A send, so the data connection carries a request as well as its
    // hello.
    let outbound = envelope(62, "out through the tap");
    world::seed_pending(&world.a, &world.b_peer, &human(), &outbound);
    let data = Tap::install(&world.a.data_socket());
    let admin = Tap::install(&world.a.admin_socket());

    let mut client = app::start(&world.a);
    let pid = i32::try_from(client.child.id()).expect("a pid");
    until_lease(&world.a.binding(), true, &client).await;
    // A receipt, so the session has done more than open.
    let mut peer = Peer::new(&world.b);
    peer.until("B ready", || world.logs(), Peer::is_ready).await;
    let message = envelope(61, "through the tap");
    peer.deliver(&world.a_peer, Some(human()), &message, || world.logs())
        .await;
    until_rows(&world.a, "unread_inbound", 1, || client.log()).await;
    peer.until(
        "the client's send reaching B",
        || world.logs(),
        |p| p.got(&outbound.app_message_id),
    )
    .await;
    assert!(client.terminate().success(), "{}", client.log());

    let on_data = asked(&data.from(pid));
    let on_admin = asked(&admin.from(pid));
    let me = i32::try_from(std::process::id()).expect("a pid");
    assert!(
        on_data.endpoint.as_deref() == Some("human") && !on_data.methods.is_empty(),
        "control: the tap read the client's lease and its send: {on_data:?}"
    );
    assert!(
        asked(&admin.from(me))
            .methods
            .iter()
            .any(|m| m.starts_with("admin.")),
        "control: the admin tap read this test's own lease queries"
    );
    let admin_named = |names: &BTreeSet<String>| names.iter().any(|n| n.starts_with("admin."));
    assert!(
        !admin_named(&on_data.capabilities) && !admin_named(&on_data.methods),
        "no admin authority asked for or used on the data socket: {on_data:?}"
    );
    let status_only: BTreeSet<String> = ["admin.status".to_owned()].into();
    assert!(
        on_admin.capabilities == status_only && on_admin.methods == status_only,
        "control: the client's status connection was seen asking for and reading status: \
         {on_admin:?}"
    );
    assert!(
        on_admin.endpoint.is_none()
            && on_admin.capabilities.is_subset(&status_only)
            && on_admin.methods.is_subset(&status_only),
        "on the admin socket only the status read, never a lease or a send: {on_admin:?}"
    );
    assert!(
        human_lease(&world.a.binding()).await.is_none(),
        "and its lease is gone"
    );
}
