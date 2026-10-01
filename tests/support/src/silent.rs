// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton

//! A peer that takes every direct v2 request and answers none: an exchange
//! sent to it stays IN FLIGHT until the sender gives up, which is what a
//! test of a shutdown's settle grace needs -- with nothing in flight a stop
//! is quick whatever grace it was given, and the test proves nothing.

use std::net::Ipv4Addr;

use futures::{AsyncReadExt as _, AsyncWriteExt as _, StreamExt as _};
use libp2p::request_response::{self, Codec, ProtocolSupport};
use libp2p::swarm::SwarmEvent;
use libp2p::{StreamProtocol, SwarmBuilder};

/// The direct protocol, spelled here rather than imported: this crate
/// depends on no product crate.
const DIRECT_PROTOCOL: StreamProtocol = StreamProtocol::new("/interweave/direct/2.0.0");

/// A running silent peer; dropping it stops it.
pub struct SilentPeer {
    /// Its `PeerId`, as text.
    pub peer: String,
    /// Where it listens, ending in `/p2p/<peer>`.
    pub address: String,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for SilentPeer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Start a silent peer listening on `ip` (a private address: ADR-0052
/// refuses a peer's loopback). Call from inside a tokio runtime.
///
/// # Panics
/// If it cannot listen.
pub async fn silent_direct_peer(ip: Ipv4Addr) -> SilentPeer {
    let mut swarm = SwarmBuilder::with_new_identity()
        .with_tokio()
        .with_tcp(
            libp2p::tcp::Config::default(),
            libp2p::noise::Config::new,
            libp2p::yamux::Config::default,
        )
        .expect("the transport stack")
        .with_behaviour(|_| {
            request_response::Behaviour::<RawCodec>::new(
                [(DIRECT_PROTOCOL, ProtocolSupport::Full)],
                request_response::Config::default(),
            )
        })
        .expect("the behaviour")
        .build();
    let peer = swarm.local_peer_id().to_string();
    swarm
        .listen_on(format!("/ip4/{ip}/tcp/0").parse().expect("an address"))
        .expect("listens");
    let address = loop {
        if let SwarmEvent::NewListenAddr { address, .. } = swarm.select_next_some().await {
            break format!("{address}/p2p/{peer}");
        }
    };
    let task = tokio::spawn(async move {
        // Every response channel is held, so no request is ever answered
        // and none fails on this side.
        let mut held = Vec::new();
        loop {
            if let SwarmEvent::Behaviour(request_response::Event::Message {
                message: request_response::Message::Request { channel, .. },
                ..
            }) = swarm.select_next_some().await
            {
                held.push(channel);
            }
        }
    });
    SilentPeer {
        peer,
        address,
        task,
    }
}

/// Reads a request's bytes whole; never writes a response.
#[derive(Clone, Default)]
struct RawCodec;

impl Codec for RawCodec {
    type Protocol = StreamProtocol;
    type Request = Vec<u8>;
    type Response = Vec<u8>;

    async fn read_request<T>(&mut self, _: &StreamProtocol, io: &mut T) -> std::io::Result<Vec<u8>>
    where
        T: futures::AsyncRead + Unpin + Send,
    {
        let mut buf = Vec::new();
        io.take(64 * 1024).read_to_end(&mut buf).await?;
        Ok(buf)
    }

    async fn read_response<T>(&mut self, _: &StreamProtocol, io: &mut T) -> std::io::Result<Vec<u8>>
    where
        T: futures::AsyncRead + Unpin + Send,
    {
        let mut buf = Vec::new();
        io.take(64 * 1024).read_to_end(&mut buf).await?;
        Ok(buf)
    }

    async fn write_request<T>(
        &mut self,
        _: &StreamProtocol,
        io: &mut T,
        req: Vec<u8>,
    ) -> std::io::Result<()>
    where
        T: futures::AsyncWrite + Unpin + Send,
    {
        io.write_all(&req).await?;
        io.close().await
    }

    async fn write_response<T>(
        &mut self,
        _: &StreamProtocol,
        io: &mut T,
        res: Vec<u8>,
    ) -> std::io::Result<()>
    where
        T: futures::AsyncWrite + Unpin + Send,
    {
        io.write_all(&res).await?;
        io.close().await
    }
}
