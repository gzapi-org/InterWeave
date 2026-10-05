// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The first two bullets: the human client and a Claude session share
//! one daemon's `PeerId` under different `EndpointId`s, and a direct message
//! reaches exactly the endpoint it names -- the default when it names
//! none -- and nothing else.

use std::time::Duration;

use interweave_human_chat_protocol::{
    HumanChatV2, decode_envelope_bytes, encode_outbound, parse_media_type,
};
use interweave_human_transport_client::Origin;
use interweave_ipc_client::IpcSession;
use interweave_local_client_api::{
    DataCapability, DataSessionBinding as _, DataSessionPort as _, SessionEvent, SessionRequest,
};
use interweave_transport_api::{
    DirectDestination, EndpointId, MAX_PAYLOAD_BYTES, MediaType, MessageId, Payload,
};

use crate::common::{PATIENCE, human};
use crate::harness::{self as app, until_lease};
use crate::world::{self, Peer, envelope, two_daemons};

fn claude() -> EndpointId {
    EndpointId::parse("claude").expect("an endpoint")
}

/// A Claude session on `home`'s daemon, holding the `claude` lease.
async fn claude_session(home: &crate::common::Home) -> IpcSession {
    home.binding()
        .open(
            SessionRequest::new(
                "claude-channel",
                Some(claude()),
                [DataCapability::Events, DataCapability::Commands],
            )
            .expect("a request"),
        )
        .await
        .expect("the claude lease")
}

/// The `HumanChatV2` payload for `envelope`, as any client would send it.
fn payload(envelope: &HumanChatV2) -> Payload {
    let encoded = encode_outbound(envelope, MAX_PAYLOAD_BYTES).expect("it fits");
    let media_type = MediaType::parse(encoded.media_type).expect("a media type");
    Payload::at_ceiling(Some(media_type), encoded.bytes).expect("within the ceiling")
}

/// Both clients send to B: what B receives names one `PeerId`, A's, under
/// two `EndpointId`s -- one per client, each the endpoint it leased.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn human_and_claude_share_one_peer_id_under_different_endpoint_ids() {
    let world = two_daemons().await;
    let from_human = envelope(1, "from the human client");
    world::seed_pending(&world.a, &world.b_peer, &human(), &from_human);
    let mut peer = Peer::new(&world.b);
    let mut app = app::start(&world.a);
    until_lease(&world.a.binding(), true, &app).await;
    let session = claude_session(&world.a).await;

    let from_claude = envelope(2, "from a Claude session");
    let deadline = tokio::time::Instant::now() + PATIENCE;
    while !peer.got(&from_claude.app_message_id) {
        // Retried until B's endpoint is reachable; one id, so B's dedup
        // folds the copies.
        let _ = session
            .send_direct(
                DirectDestination {
                    peer: world.b_peer.clone(),
                    endpoint: Some(human()),
                },
                MessageId::from_bytes([2; 16]),
                payload(&from_claude),
            )
            .await;
        peer.step().await;
        assert!(
            tokio::time::Instant::now() < deadline,
            "B never got the Claude session's message\n{}",
            world.logs()
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    peer.until(
        "the human client's message reaching B",
        || world.logs(),
        |p| p.got(&from_human.app_message_id),
    )
    .await;

    let origin = |id: &str| {
        peer.received
            .iter()
            .find(|r| r.envelope.app_message_id == id)
            .map(|r| r.origin.clone())
            .expect("received")
    };
    assert_eq!(
        origin(&from_human.app_message_id),
        Origin::Direct {
            peer: world.a_peer.clone(),
            endpoint: human(),
        },
        "the human client sends as A's PeerId, from `human`"
    );
    assert_eq!(
        origin(&from_claude.app_message_id),
        Origin::Direct {
            peer: world.a_peer.clone(),
            endpoint: claude(),
        },
        "the Claude session sends as the same PeerId, from `claude`"
    );
    assert!(app.running(), "{}", app.report());
    session.close().await.expect("closed");
    assert!(app.terminate().success(), "{}", app.log());
}

/// B sends to A three ways -- the default endpoint, `human` by name,
/// `claude` by name. The app keeps exactly the two meant for `human`, the
/// Claude session sees exactly the one meant for it, and neither sees a
/// copy of the other's: no fan-out.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn direct_messages_route_to_exactly_one_endpoint_with_no_fan_out() {
    let world = two_daemons().await;
    let mut peer = Peer::new(&world.b);
    let app = app::start(&world.a);
    until_lease(&world.a.binding(), true, &app).await;
    let session = claude_session(&world.a).await;
    peer.until("B ready", || world.logs(), Peer::is_ready).await;

    let to_default = envelope(11, "to A's default endpoint");
    let to_human = envelope(12, "to A's human endpoint");
    let to_claude = envelope(13, "to A's claude endpoint");
    peer.send(&world.a_peer, None, &to_default).await;
    peer.send(&world.a_peer, Some(human()), &to_human).await;
    peer.send(&world.a_peer, Some(claude()), &to_claude).await;
    peer.until(
        "all three accepted",
        || world.logs(),
        |p| {
            [&to_default, &to_human, &to_claude]
                .iter()
                .all(|e| p.accepted(&e.app_message_id))
        },
    )
    .await;

    // What the Claude session is handed, decoded, until the human
    // client's two are in its store; then a settle, for a copy that is
    // late rather than absent.
    let mut at_claude: Vec<(EndpointId, String)> = Vec::new();
    let take = async |at: &mut Vec<(EndpointId, String)>| {
        for event in session.events(64).await.expect("events") {
            if let SessionEvent::Direct(direct) = event {
                let media_type = direct.payload.media_type().expect("a media type");
                let info = parse_media_type(media_type.as_str()).expect("HumanChatV2");
                let text =
                    decode_envelope_bytes(direct.payload.bytes(), info.encoding).expect("decodes");
                let envelope = HumanChatV2::parse(&text).expect("an envelope");
                at.push((direct.destination_endpoint, envelope.app_message_id));
            }
        }
    };
    world::until_rows(&world.a, "unread_inbound", 2, || {
        format!("{}\n{}", app.log(), world.logs())
    })
    .await;
    let settle = tokio::time::Instant::now() + Duration::from_secs(2);
    while tokio::time::Instant::now() < settle {
        take(&mut at_claude).await;
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    assert_eq!(
        at_claude,
        [(claude(), to_claude.app_message_id.clone())],
        "the Claude session got its one message and nothing meant for `human`"
    );
    assert_eq!(
        world::ids(&world.a, "unread_inbound"),
        [&to_default, &to_human]
            .iter()
            .map(|e| e.app_message_id.clone())
            .collect(),
        "the human client kept the default's and `human`'s, and not `claude`'s"
    );
    session.close().await.expect("closed");
}
