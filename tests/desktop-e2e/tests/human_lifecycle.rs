// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The desktop client's life against a real daemon, on a real display
//! (plan section 18, batch 4): it takes the `human` endpoint's lease once
//! it can, gives it back when it ends, and never stops the daemon
//! (ADR-0040). What it does NOT prove: anything a person does in the
//! window -- batches 5 to 7.

#![allow(clippy::expect_used, clippy::panic)]

mod common;
mod human_app;

use std::net::Ipv4Addr;
use std::time::Duration;

use common::{Home, example, free_port, lease_request, stranger};
use interweave_local_client_api::{DataSessionBinding as _, DataSessionPort as _};

fn home() -> Home {
    let home = Home::new("human-desktop");
    home.write_key();
    let listen = format!("/ip4/127.0.0.1/tcp/{}", free_port(Ipv4Addr::LOCALHOST));
    home.write_config(&example("human-desktop.yaml", &stranger(), &listen, None));
    home
}

#[tokio::test(flavor = "multi_thread")]
async fn a_signal_ends_the_client_releasing_its_lease_and_leaving_the_daemon_serving() {
    let home = home();
    let mut daemon = home.start(&[]);
    daemon.serving(&home).await;
    let binding = home.binding();

    let mut app = human_app::start(&home);
    human_app::until_lease(&binding, true, &app).await;

    let status = app.terminate();
    assert!(
        status.success(),
        "exit 0 on SIGTERM: {status:?}: {}",
        app.log()
    );
    human_app::until_lease(&binding, false, &app).await;
    assert!(
        daemon.child.try_wait().expect("a status").is_none(),
        "the daemon is still running: {}",
        daemon.log()
    );
    let session = binding
        .open(lease_request())
        .await
        .expect("the human endpoint can be leased again");
    session.close().await.expect("closed");
}

#[tokio::test(flavor = "multi_thread")]
async fn started_before_its_daemon_the_client_waits_and_then_connects() {
    let home = home();
    let mut app = human_app::start(&home);
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert!(app.running(), "no daemon is not a crash: {}", app.log());

    let mut daemon = home.start(&[]);
    daemon.serving(&home).await;
    human_app::until_lease(&home.binding(), true, &app).await;

    assert!(app.terminate().success(), "{}", app.log());
}
