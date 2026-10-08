// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton

//! The upgrade matrix's broadcast rows (testing.md §Compatibility
//! fixtures, A 2026-10-08):
//!
//! - **unsupported major**: an envelope whose version byte is not 1 is
//!   objective invalidity, `Reject` under ADR-0029. It is never `Ignore`
//!   and never delivered. Over real sockets, a raw publisher that signs
//!   with its own trusted key sends HEAD envelopes of versions 0, 2 and
//!   255, then a version-1 control, and HEAD's session receives exactly the
//!   control. The wire cannot tell `Reject` from `Ignore` (both are
//!   withheld from the session), so the verdict itself is asserted on the
//!   function whose verdict HEAD reports to gossipsub, for those same bytes
//!   and the same trusted publisher.
//! - **minor bump**: NOT APPLICABLE. The envelope's version byte is a
//!   major and no minor exists (PUBSUB.md §Envelope; the row's text in
//!   testing.md). There is nothing to test, and this file says so rather
//!   than leaving the row silent.
//!
//! Every envelope is encoded by the independent codec.

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::time::Duration;

use futures::StreamExt as _;
use interweave_independent_codecs::broadcast_v1::BroadcastMessageV1 as IndependentEnvelope;
use interweave_local_client_api::{
    DataCapability, DataSessionBinding as _, DataSessionPort as _, SessionEvent, SessionRequest,
};
use interweave_local_client_conformance_tests::{PATIENCE, receive};
use interweave_profile_config::ProfileConfig;
use interweave_profile_identity::ProfileIdentity;
use interweave_transport_api::{
    ChannelId, MAX_PAYLOAD_BYTES, TransportIdentity, TransportRuntime as _,
};
use interweave_transport_composition::{ComposedRuntime, CompositionOptions};
use interweave_transport_runtime::{ProtocolVerdict, classify_broadcast, topic::topic_key_v1};
use interweave_trust_api::PeerTrustPolicy;

fn envelope(version: u8, payload: &[u8]) -> Vec<u8> {
    let mut bytes = IndependentEnvelope {
        message_id: [0x3a; 16],
        sent_at_ms: 1_786_600_000_000,
        media_type: Some("text/plain".to_owned()),
        payload: payload.to_vec(),
    }
    .encode()
    .expect("a legal envelope");
    // The same layout under another version byte: what a build of another
    // major would put on the topic, as far as a 1.x reader can know.
    bytes[0] = version;
    bytes
}

const FOREIGN: [u8; 3] = [0, 2, 255];

/// The verdict HEAD reports for each envelope, from a trusted publisher:
/// `Reject` for every foreign version, `Accept` for the control. Asserted
/// here because the wire cannot distinguish `Reject` from `Ignore`.
#[test]
fn a_foreign_version_byte_is_reject_never_ignore() {
    let publisher = ProfileIdentity::generate()
        .transport_identity()
        .expect("peer");
    let trust = PeerTrustPolicy::new([publisher.clone()]).expect("one peer");
    assert!(matches!(
        classify_broadcast(
            &envelope(1, b"control"),
            MAX_PAYLOAD_BYTES,
            &publisher,
            &trust
        ),
        ProtocolVerdict::Accept(_)
    ));
    for v in FOREIGN {
        assert!(
            matches!(
                classify_broadcast(
                    &envelope(v, b"foreign"),
                    MAX_PAYLOAD_BYTES,
                    &publisher,
                    &trust
                ),
                ProtocolVerdict::Reject
            ),
            "version {v}"
        );
    }
}

fn profile(trusted: &TransportIdentity) -> ProfileConfig {
    let doc = format!(
        "schema_version: 2
trust:
  policy: static-allowlist
  allowed_peers: [\"{}\"]
endpoints:
  default_direct_endpoint: human
  entries:
    - id: human
      enabled: true
      advertise: false
channels:
  desired: []
discovery:
  providers: []
",
        trusted.as_str()
    );
    serde_norway::from_str(&doc).expect("the document parses")
}

/// Over the wire: the foreign versions are withheld, the control arrives,
/// and the foreign ones did not wedge it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_foreign_version_byte_is_not_delivered_beside_a_version_1_control() {
    let ip = interweave_test_support::net::require_private_interface_v4();
    let keys = libp2p::identity::Keypair::generate_ed25519();
    let publisher =
        TransportIdentity::parse(libp2p::PeerId::from_public_key(&keys.public()).to_base58())
            .expect("a canonical peer id");

    let head = ComposedRuntime::start(
        &ProfileIdentity::generate(),
        &profile(&publisher),
        CompositionOptions {
            listen: vec![format!("/ip4/{ip}/tcp/0")],
            ..CompositionOptions::default()
        },
    )
    .await
    .expect("HEAD composes");
    let address: libp2p::Multiaddr = head.listening()[0].parse().expect("an address");
    let channel = ChannelId::parse("interop").expect("valid");
    let session = head
        .sessions()
        .open(
            SessionRequest::new(
                "human-client",
                Some(interweave_transport_api::EndpointId::parse("human").expect("valid")),
                [DataCapability::Commands, DataCapability::Events],
            )
            .expect("in bounds"),
        )
        .await
        .expect("leases");
    session.join(channel.clone()).await.expect("joins");

    let topic = libp2p::gossipsub::IdentTopic::new(topic_key_v1(&channel).wire_string());
    let publisher_task = tokio::spawn(async move {
        let mut swarm = libp2p::SwarmBuilder::with_existing_identity(keys)
            .with_tokio()
            .with_tcp(
                libp2p::tcp::Config::default(),
                libp2p::noise::Config::new,
                libp2p::yamux::Config::default,
            )
            .expect("the transport stack")
            .with_behaviour(|keys| {
                libp2p::gossipsub::Behaviour::<
                    libp2p::gossipsub::IdentityTransform,
                    libp2p::gossipsub::AllowAllSubscriptionFilter,
                >::new(
                    // Signed by its own trusted key: everything but the
                    // version byte is valid, so the verdict comes from it.
                    libp2p::gossipsub::MessageAuthenticity::Signed(keys.clone()),
                    libp2p::gossipsub::ConfigBuilder::default()
                        .build()
                        .expect("a default config"),
                )
                .expect("the behaviour builds")
            })
            .expect("behaviour")
            .build();
        swarm.behaviour_mut().subscribe(&topic).expect("subscribes");
        swarm.dial(address).expect("dials HEAD");
        // Each foreign version repeatedly while the mesh forms, then the
        // control; a distinct payload each round keeps every message's id
        // distinct.
        for round in 0u8..40 {
            let body = if round < 30 {
                envelope(FOREIGN[usize::from(round) % FOREIGN.len()], &[b'f', round])
            } else {
                envelope(1, &[b'c', round])
            };
            let _ = swarm.behaviour_mut().publish(topic.hash(), body);
            let _ = tokio::time::timeout(Duration::from_millis(50), swarm.select_next_some()).await;
        }
    });
    publisher_task.await.expect("the publisher ran");

    let got = receive(&session, PATIENCE).await;
    // A session notice (the runtime's state) may ride along; only the
    // broadcasts are this row's.
    let broadcasts: Vec<_> = got
        .iter()
        .filter_map(|e| match e {
            SessionEvent::Broadcast(m) => Some(m),
            SessionEvent::Direct(_) => panic!("no direct traffic here: {got:?}"),
            SessionEvent::Local(_) => None,
        })
        .collect();
    assert!(
        !broadcasts.is_empty(),
        "the control never arrived, so the harness proved nothing: {got:?}"
    );
    for message in broadcasts {
        assert_eq!(
            message.payload.bytes()[0],
            b'c',
            "only version-1 envelopes are delivered: {got:?}"
        );
    }

    session.close().await.expect("closes");
    head.shutdown().await.expect("HEAD stops");
}
