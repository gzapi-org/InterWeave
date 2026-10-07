// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! `human-client-ui.md` §13's trust-mutation bullet in the shipped client
//! (plan §18, B9): the trust settings show the exact target `PeerId`, the
//! change is made only on its confirmation, and it reaches the daemon
//! over the admin socket alone -- never the data socket, whatever the
//! client asks there.
//!
//! What this does not prove: an allow typed into the field (its view and
//! root tests are `ui-slint`'s and `app-core`'s), or that a change outlives
//! the daemon -- by decision it does not (ADR-0028), and the view says so.

use interweave_human_ui_model::{UiText, fill, placeholder_en};
use interweave_local_client_api::{AdminBinding as _, AdminCapability, AdminPort as _};

use crate::a11y::{Bus, Element};
use crate::daemon_link::{Tap, asked};
use crate::display;
use crate::harness::{self as app, until_lease};
use crate::world::two_daemons;

fn text(key: UiText) -> &'static str {
    placeholder_en::text(key)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_trust_removal_shows_the_exact_peer_id_and_reaches_the_daemon_only_over_admin() {
    let _focus = display::exclusive().await;
    let world = two_daemons().await;
    let data = Tap::install(&world.a.data_socket());
    let admin = Tap::install(&world.a.admin_socket());
    let bus = Bus::connect().await;
    let mut client = app::start(&world.a);
    until_lease(&world.a.binding(), true, &client).await;
    let pid = i32::try_from(client.child.id()).expect("a pid");
    let window = bus.window_of(client.child.id(), || client.log()).await;
    display::focus(client.child.id(), || client.log()).await;
    let log = || client.log();
    let button = |name: &'static str| {
        move |e: &Element| e.role == "button" && e.name == name && e.actions == ["click"]
    };

    let open = window
        .until(
            "the trust settings' control",
            button(text(UiText::TrustSettings)),
            log,
        )
        .await;
    window.activate(&open, "click").await;

    // B's PeerId whole, on its row and on its removal's description; this
    // profile's own labelled beside them.
    let b = world.b_peer.as_str();
    window
        .until(
            "B's PeerId, whole, on its row",
            |e: &Element| e.name == b,
            log,
        )
        .await;
    let remove = window
        .until(
            "B's removal, naming B",
            |e: &Element| {
                e.role == "button" && e.name == text(UiText::RemoveTrust) && e.description == b
            },
            log,
        )
        .await;
    // Section 13's accessibility bullet, its trust leg: every trust control
    // is labelled for what it does, and each button can be pressed.
    let tree = window.read().await;
    let has = |role: &str, name: &str, actions: &[&str]| {
        tree.iter()
            .any(|e| e.role == role && e.name == name && e.actions == actions)
    };
    for (role, name, actions) in [
        ("entry", text(UiText::OwnPeerId), &[][..]),
        ("entry", text(UiText::PeerIdToTrust), &[][..]),
        ("button", text(UiText::TrustPeer), &["click"][..]),
        ("button", text(UiText::BackToConversations), &["click"][..]),
    ] {
        assert!(
            has(role, name, actions),
            "{role} {name:?} {actions:?} in the tree:\n{}",
            crate::a11y::describe(&tree)
        );
    }
    window.activate(&remove, "click").await;

    // The confirmation names the exact PeerId and what follows; nothing
    // has reached the daemon yet.
    let question = fill(text(UiText::ConfirmRevoke), &[("peer", b)]);
    window
        .until(
            "the confirmation, naming B whole",
            |e: &Element| e.name == question,
            log,
        )
        .await;
    assert!(
        !asked(&admin.from(pid)).methods.contains("admin.trust.set"),
        "a proposal asks the daemon nothing"
    );
    let confirm = window
        .until(
            "the confirmation's Confirm, describing it",
            |e: &Element| {
                e.role == "button"
                    && e.name == text(UiText::ConfirmChange)
                    && e.description == question
            },
            log,
        )
        .await;
    window.activate(&confirm, "click").await;
    let done = fill(text(UiText::PeerUntrusted), &[("peer", b)]);
    window
        .until("the outcome, naming B", |e: &Element| e.name == done, log)
        .await;

    // The daemon holds it: B is no longer allowed, read by this test on
    // its own admin connection.
    let port = world
        .a
        .binding()
        .admin([AdminCapability::Trust].into())
        .await
        .expect("an admin port");
    let view = port.trust().await.expect("the allowlist");
    assert_eq!(view.local_peer.as_ref(), Some(&world.a_peer));
    assert!(
        !view.peers().any(|p| p == &world.b_peer),
        "the daemon no longer allows B: {view:?}"
    );

    // Over the admin socket and nowhere else: the trust capability and the
    // set on the client's admin connections, nothing administrative asked
    // or sent on its data connection. Control: the data connection was
    // seen leasing its endpoint.
    assert!(client.terminate().success(), "{}", client.log());
    let on_admin = asked(&admin.from(pid));
    let on_data = asked(&data.from(pid));
    assert!(
        on_admin.capabilities.contains("admin.trust")
            && on_admin.methods.contains("admin.trust.set"),
        "the trust set went over the admin socket: {on_admin:?}"
    );
    assert_eq!(
        on_data.endpoint.as_deref(),
        Some("human"),
        "control: the data tap read the client's lease: {on_data:?}"
    );
    let administrative =
        |names: &std::collections::BTreeSet<String>| names.iter().any(|n| n.starts_with("admin."));
    assert!(
        !administrative(&on_data.capabilities) && !administrative(&on_data.methods),
        "no administrative authority asked for or used on the data socket: {on_data:?}"
    );
}
