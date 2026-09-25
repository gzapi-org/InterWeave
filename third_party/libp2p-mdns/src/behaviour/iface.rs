// Copyright 2018 Parity Technologies (UK) Ltd.
//
// Permission is hereby granted, free of charge, to any person obtaining a
// copy of this software and associated documentation files (the "Software"),
// to deal in the Software without restriction, including without limitation
// the rights to use, copy, modify, merge, publish, distribute, sublicense,
// and/or sell copies of the Software, and to permit persons to whom the
// Software is furnished to do so, subject to the following conditions:
//
// The above copyright notice and this permission notice shall be included in
// all copies or substantial portions of the Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS
// OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
// FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
// AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
// LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING
// FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER
// DEALINGS IN THE SOFTWARE.

mod dns;
mod query;

use std::{
    collections::VecDeque,
    future::Future,
    io,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, UdpSocket},
    pin::Pin,
    sync::{Arc, RwLock},
    task::{Context, Poll},
    time::{Duration, Instant},
};

use futures::{SinkExt, StreamExt, channel::mpsc};
use libp2p_core::{Multiaddr, multiaddr::Protocol};
use libp2p_identity::PeerId;
use libp2p_swarm::ListenAddresses;
use socket2::{Domain, Socket, Type};

use self::{
    dns::{build_query, build_query_response, build_service_discovery_response},
    query::MdnsPacket,
};
use crate::{
    Config,
    behaviour::{DropCounts, socket::AsyncSocket, timer::Builder},
};

/// INTERWEAVE PATCH (ADR-0053 rule 4): the answers this node multicasts,
/// each rate-limited on its own slot.
#[derive(Debug, Clone, Copy)]
enum Answer {
    /// The peer records: the PTR for `_p2p._udp.local` and its TXTs.
    Peer = 0,
    /// The service-discovery (meta-query) answer.
    Service = 1,
}

/// Initial interval for starting probe
const INITIAL_TIMEOUT_INTERVAL: Duration = Duration::from_millis(500);

#[derive(Debug, Clone)]
enum ProbeState {
    Probing(Duration),
    Finished(Duration),
}

impl Default for ProbeState {
    fn default() -> Self {
        ProbeState::Probing(INITIAL_TIMEOUT_INTERVAL)
    }
}

impl ProbeState {
    fn interval(&self) -> &Duration {
        match self {
            ProbeState::Probing(query_interval) => query_interval,
            ProbeState::Finished(query_interval) => query_interval,
        }
    }
}

/// An mDNS instance for a networking interface. To discover all peers when having multiple
/// interfaces an [`InterfaceState`] is required for each interface.
#[derive(Debug)]
pub(crate) struct InterfaceState<U, T> {
    /// Address this instance is bound to.
    addr: IpAddr,
    /// Receive socket.
    recv_socket: U,
    /// Send socket.
    send_socket: U,

    listen_addresses: Arc<RwLock<ListenAddresses>>,

    query_response_sender: mpsc::Sender<(PeerId, Multiaddr, Instant)>,

    /// Buffer used for receiving data from the main socket.
    /// RFC6762 discourages packets larger than the interface MTU, but allows sizes of up to 9000
    /// bytes, if it can be ensured that all participating devices can handle such large packets.
    /// For computers with several interfaces and IP addresses responses can easily reach sizes in
    /// the range of 3000 bytes, so 4096 seems sensible for now. For more information see
    /// [rfc6762](https://tools.ietf.org/html/rfc6762#page-46).
    recv_buffer: [u8; 4096],
    /// Buffers pending to send on the main socket.
    ///
    /// INTERWEAVE PATCH (ADR-0053 rule 4): each tagged with the answer it
    /// carries, if any, so the answer's slot is stamped when it LEAVES the
    /// buffer, sent or failed -- never when it is queued.
    send_buffer: VecDeque<(Option<Answer>, Vec<u8>)>,
    /// Discovery interval.
    query_interval: Duration,
    /// Discovery timer.
    timeout: T,
    /// Multicast address.
    multicast_addr: IpAddr,
    /// Discovered addresses.
    discovered: VecDeque<(PeerId, Multiaddr, Instant)>,
    /// TTL
    ttl: Duration,
    probe_state: ProbeState,
    local_peer_id: PeerId,
    /// INTERWEAVE PATCH (ADR-0053 rules 4, 5, 7): when this interface last
    /// sent, or failed to send, each of its answers, where it reports a failure, and what it
    /// dropped. One slot PER ANSWER (RFC 6762 section 6: a given record on
    /// a given interface once a second), indexed by `Answer`, so a
    /// meta-query cannot spend the slot the peer answer needs.
    last_answer: [Option<Instant>; 2],
    /// Packets of each answer queued and not yet sent: a query whose
    /// answer is already on its way is counted, not answered twice.
    queued_answers: [usize; 2],
    failure_sender: mpsc::Sender<(IpAddr, String)>,
    drop_counts: Arc<DropCounts>,
}

impl<U, T> InterfaceState<U, T>
where
    U: AsyncSocket,
    T: Builder + futures::Stream,
{
    /// Builds a new [`InterfaceState`].
    pub(crate) fn new(
        addr: IpAddr,
        config: Config,
        local_peer_id: PeerId,
        listen_addresses: Arc<RwLock<ListenAddresses>>,
        query_response_sender: mpsc::Sender<(PeerId, Multiaddr, Instant)>,
        failure_sender: mpsc::Sender<(IpAddr, String)>,
        drop_counts: Arc<DropCounts>,
    ) -> io::Result<Self> {
        tracing::info!(address=%addr, "creating instance on iface address");
        let recv_socket = match addr {
            IpAddr::V4(addr) => {
                let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(socket2::Protocol::UDP))?;
                socket.set_reuse_address(true)?;
                #[cfg(unix)]
                socket.set_reuse_port(true)?;
                socket.bind(&SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 5353).into())?;
                socket.set_multicast_loop_v4(true)?;
                socket.set_multicast_ttl_v4(255)?;
                socket.join_multicast_v4(&crate::IPV4_MDNS_MULTICAST_ADDRESS, &addr)?;
                U::from_std(UdpSocket::from(socket))?
            }
            IpAddr::V6(_) => {
                let socket = Socket::new(Domain::IPV6, Type::DGRAM, Some(socket2::Protocol::UDP))?;
                socket.set_reuse_address(true)?;
                #[cfg(unix)]
                socket.set_reuse_port(true)?;
                socket.bind(&SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 5353).into())?;
                socket.set_multicast_loop_v6(true)?;
                // TODO: find interface matching addr.
                socket.join_multicast_v6(&crate::IPV6_MDNS_MULTICAST_ADDRESS, 0)?;
                U::from_std(UdpSocket::from(socket))?
            }
        };
        let bind_addr = match addr {
            IpAddr::V4(_) => SocketAddr::new(addr, 0),
            IpAddr::V6(_addr) => {
                // TODO: if-watch should return the scope_id of an address
                // as a workaround we bind to unspecified, which means that
                // this probably won't work when using multiple interfaces.
                // SocketAddr::V6(SocketAddrV6::new(addr, 0, 0, scope_id))
                SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 0)
            }
        };
        let send_socket = U::from_std(UdpSocket::bind(bind_addr)?)?;

        // randomize timer to prevent all converging and firing at the same time.
        let query_interval = {
            let jitter = rand::random_range(0..100);
            config.query_interval + Duration::from_millis(jitter)
        };
        let multicast_addr = match addr {
            IpAddr::V4(_) => IpAddr::V4(crate::IPV4_MDNS_MULTICAST_ADDRESS),
            IpAddr::V6(_) => IpAddr::V6(crate::IPV6_MDNS_MULTICAST_ADDRESS),
        };
        Ok(Self {
            addr,
            recv_socket,
            send_socket,
            listen_addresses,
            query_response_sender,
            recv_buffer: [0; 4096],
            send_buffer: Default::default(),
            discovered: Default::default(),
            query_interval,
            timeout: T::interval_at(Instant::now(), INITIAL_TIMEOUT_INTERVAL),
            multicast_addr,
            ttl: config.ttl,
            probe_state: Default::default(),
            local_peer_id,
            last_answer: [None, None],
            queued_answers: [0, 0],
            failure_sender,
            drop_counts,
        })
    }

    /// INTERWEAVE PATCH (ADR-0053 rule 2): queue a packet unless the send
    /// buffer is full, in which case it is dropped and counted -- and, for
    /// an answer, its slot is not touched, so a dropped answer does not
    /// silence the next query.
    fn queue_packet(&mut self, answer: Option<Answer>, packet: Vec<u8>) {
        if self.send_buffer.len() >= crate::MAX_INTERFACE_SEND_PACKETS {
            DropCounts::count(self.drop_counts.packets_dropped_counter());
            return;
        }
        if let Some(answer) = answer {
            self.queued_answers[answer as usize] += 1;
        }
        self.send_buffer.push_back((answer, packet));
    }

    /// INTERWEAVE PATCH (ADR-0053 rule 4): a packet left the send buffer,
    /// sent or failed. An answer's slot is stamped when a packet of it
    /// LEAVES, not when it is queued, so a socket that stalls cannot let
    /// answers queued a second apart go out back to back (#112, the
    /// automated review's P2 on fd10e50). A failed send is stamped too:
    /// it is an attempt, and each one is an `InterfaceFailed` (rule 5), so
    /// an unstamped failure let a remote host's query rate set this
    /// node's failure-event rate (#112 blind review, on 34fd3ad).
    fn packet_left(&mut self, answer: Option<Answer>) {
        if let Some(answer) = answer {
            let i = answer as usize;
            self.queued_answers[i] = self.queued_answers[i].saturating_sub(1);
            self.last_answer[i] = Some(Instant::now());
        }
    }

    /// INTERWEAVE PATCH (ADR-0053 rule 4): whether this interface may
    /// send `answer` now -- each answer at most once per
    /// `MIN_ANSWER_INTERVAL` (RFC 6762 section 6). A refused answer is
    /// counted.
    fn may_answer(&mut self, answer: Answer) -> bool {
        let now = Instant::now();
        let i = answer as usize;
        let recently = self.last_answer[i]
            .is_some_and(|last| now.duration_since(last) < crate::MIN_ANSWER_INTERVAL);
        if recently || self.queued_answers[i] > 0 {
            DropCounts::count(self.drop_counts.queries_unanswered_counter());
            return false;
        }
        true
    }

    /// INTERWEAVE PATCH (ADR-0053 rule 5): report a failure to the
    /// behaviour, which turns it into `Event::InterfaceFailed`. A full
    /// channel loses the report, counted.
    fn report_failure(&mut self, reason: String) {
        if self.failure_sender.try_send((self.addr, reason)).is_err() {
            DropCounts::count(self.drop_counts.failures_dropped_counter());
        }
    }

    pub(crate) fn reset_timer(&mut self) {
        tracing::trace!(address=%self.addr, probe_state=?self.probe_state, "reset timer");
        let interval = *self.probe_state.interval();
        self.timeout = T::interval(interval);
    }

    fn mdns_socket(&self) -> SocketAddr {
        SocketAddr::new(self.multicast_addr, 5353)
    }
}

impl<U, T> Future for InterfaceState<U, T>
where
    U: AsyncSocket,
    T: Builder + futures::Stream,
{
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();

        loop {
            // 1st priority: Low latency: Create packet ASAP after timeout.
            if this.timeout.poll_next_unpin(cx).is_ready() {
                tracing::trace!(address=%this.addr, "sending query on iface");
                this.queue_packet(None, build_query());
                tracing::trace!(address=%this.addr, probe_state=?this.probe_state, "tick");

                // Stop to probe when the initial interval reach the query interval
                if let ProbeState::Probing(interval) = this.probe_state {
                    let interval = interval * 2;
                    this.probe_state = if interval >= this.query_interval {
                        ProbeState::Finished(this.query_interval)
                    } else {
                        ProbeState::Probing(interval)
                    };
                }

                this.reset_timer();
            }

            // 2nd priority: Keep local buffers small: Send packets to remote.
            if let Some((answer, packet)) = this.send_buffer.pop_front() {
                match this.send_socket.poll_write(cx, &packet, this.mdns_socket()) {
                    Poll::Ready(Ok(_)) => {
                        tracing::trace!(address=%this.addr, "sent packet on iface address");
                        this.packet_left(answer);
                        continue;
                    }
                    Poll::Ready(Err(err)) => {
                        tracing::error!(address=%this.addr, "error sending packet on iface address {}", err);
                        this.packet_left(answer);
                        this.report_failure(err.to_string());
                        continue;
                    }
                    Poll::Pending => {
                        this.send_buffer.push_front((answer, packet));
                    }
                }
            }

            // 3rd priority: Keep local buffers small: Return discovered addresses.
            if this.query_response_sender.poll_ready_unpin(cx).is_ready()
                && let Some(discovered) = this.discovered.pop_front()
            {
                match this.query_response_sender.try_send(discovered) {
                    Ok(()) => {}
                    Err(e) if e.is_disconnected() => {
                        return Poll::Ready(());
                    }
                    Err(e) => {
                        this.discovered.push_front(e.into_inner());
                    }
                }

                continue;
            }

            // 4th priority: Remote work: Answer incoming requests.
            match this
                .recv_socket
                .poll_read(cx, &mut this.recv_buffer)
                .map_ok(|(len, from)| MdnsPacket::new_from_bytes(&this.recv_buffer[..len], from))
            {
                Poll::Ready(Ok(Ok(Some(MdnsPacket::Query(query))))) => {
                    tracing::trace!(
                        address=%this.addr,
                        remote_address=%query.remote_addr(),
                        "received query from remote address on address"
                    );

                    // INTERWEAVE PATCH (ADR-0053 rule 4): the peer answer
                    // once a second on this interface; the rest counted.
                    if !this.may_answer(Answer::Peer) {
                        continue;
                    }
                    // Only send addresses that belong to this interface.
                    // This prevents advertising loopback or other interface addresses
                    // to peers that can't reach them.
                    let iface_ip = this.addr;
                    let read = this
                        .listen_addresses
                        .read()
                        .unwrap_or_else(|e| e.into_inner());
                    let relevant_addrs = read
                        .iter()
                        .filter(|multiaddr| addr_matches_interface(multiaddr, iface_ip))
                        .collect();

                    let packets = build_query_response(
                        query.query_id(),
                        this.local_peer_id,
                        relevant_addrs,
                        this.ttl,
                    );
                    drop(read);
                    for packet in packets {
                        this.queue_packet(Some(Answer::Peer), packet);
                    }
                    continue;
                }
                Poll::Ready(Ok(Ok(Some(MdnsPacket::Response(response))))) => {
                    tracing::trace!(
                        address=%this.addr,
                        remote_address=%response.remote_addr(),
                        "received response from remote address on address"
                    );

                    // INTERWEAVE PATCH (ADR-0053 rule 2): the queue is
                    // bounded; past it a pair is dropped and counted.
                    for pair in response.extract_discovered(Instant::now(), this.local_peer_id) {
                        if this.discovered.len() >= crate::MAX_INTERFACE_DISCOVERED {
                            DropCounts::count(this.drop_counts.discovered_dropped_counter());
                            continue;
                        }
                        this.discovered.push_back(pair);
                    }

                    // Stop probing when we have a valid response
                    if !this.discovered.is_empty() {
                        this.probe_state = ProbeState::Finished(this.query_interval);
                        this.reset_timer();
                    }
                    continue;
                }
                Poll::Ready(Ok(Ok(Some(MdnsPacket::ServiceDiscovery(disc))))) => {
                    tracing::trace!(
                        address=%this.addr,
                        remote_address=%disc.remote_addr(),
                        "received service discovery from remote address on address"
                    );

                    // INTERWEAVE PATCH (ADR-0053 rule 4): the service
                    // answer, on its own slot.
                    if !this.may_answer(Answer::Service) {
                        continue;
                    }
                    this.queue_packet(
                        Some(Answer::Service),
                        build_service_discovery_response(disc.query_id(), this.ttl),
                    );
                    continue;
                }
                Poll::Ready(Err(err)) if err.kind() == std::io::ErrorKind::WouldBlock => {
                    // No more bytes available on the socket to read
                    continue;
                }
                Poll::Ready(Err(err)) => {
                    tracing::error!("failed reading datagram: {}", err);
                    // INTERWEAVE PATCH (ADR-0053 rule 5): this ends the
                    // interface's task; say so before it goes.
                    this.report_failure(err.to_string());
                    return Poll::Ready(());
                }
                Poll::Ready(Ok(Err(err))) => {
                    tracing::debug!("Parsing mdns packet failed: {:?}", err);
                    continue;
                }
                Poll::Ready(Ok(Ok(None))) => continue,
                Poll::Pending => {}
            }

            return Poll::Pending;
        }
    }
}

/// Returns `true` if the first protocol component of `addr` is an IP address equal to
/// `iface_ip`.
///
/// Used when answering mDNS queries to advertise only the listening addresses that are
/// reachable on the interface the query arrived on, instead of every listening address.
fn addr_matches_interface(addr: &Multiaddr, iface_ip: IpAddr) -> bool {
    match addr.iter().next() {
        Some(Protocol::Ip4(v4)) => IpAddr::V4(v4) == iface_ip,
        Some(Protocol::Ip6(v6)) => IpAddr::V6(v6) == iface_ip,
        _ => false,
    }
}
