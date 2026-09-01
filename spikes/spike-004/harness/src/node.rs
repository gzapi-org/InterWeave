// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton

//! One spike node: the three Stage 11 behaviours behind the production
//! root gate.
//!
//! Every dialling behaviour is wrapped in [`Attributing`] so the gate
//! is told which one asked. That wrapper is the mechanism under test,
//! not a convenience: without it the gate sees an unticketed dial and
//! nothing else, which is exactly the state production is in today and
//! why `kademlia_is_still_the_only_behaviour_that_can_originate_a_dial`
//! exists to fail when a second dialler appears.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use interweave_transport_api::TransportIdentity;
use interweave_transport_runtime::{
    ConnectionManager, ConnectionPolicy, DialOrigin, TrustSources,
};
use interweave_trust_api::{InfrastructureSet, PeerTrustPolicy};
use libp2p::swarm::NetworkBehaviour;
use libp2p::{Multiaddr, PeerId, Swarm, identify, identity, noise, relay, tcp, yamux};

use std::collections::BTreeSet;

use crate::attribute::{Attributing, Attribution, Classifier, always};
use crate::gate::{DialLedger, InstrumentedGate};

/// Classify a relay-client dial: to a configured relay it is a
/// reservation, to anyone else a circuit toward that peer.
fn relay_classifier(relays: Arc<Mutex<BTreeSet<PeerId>>>) -> Classifier {
    Arc::new(move |peer| {
        let known = relays.lock().unwrap_or_else(|e| e.into_inner());
        match peer {
            Some(p) if known.contains(&p) => DialOrigin::RelayReservation,
            Some(_) => DialOrigin::RelayCircuit,
            // A dial naming no peer cannot be a circuit toward one.
            None => DialOrigin::RelayReservation,
        }
    })
}

/// Pending dials one spike node may hold at once.
const SPIKE_MAX_PENDING_DIALS: usize = 32;

/// Established connections one spike node may hold at once.
const SPIKE_MAX_CONNECTIONS: usize = 32;

/// Which roles a node runs. The spike builds nodes asymmetrically —
/// a client that probes and reserves, a server that answers — because
/// that is the deployment shape ADR-0035 describes, and a symmetric
/// node would not show a refusal that only a server can make.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Roles {
    pub autonat_client: bool,
    pub autonat_server: bool,
    pub relay_client: bool,
    pub relay_server: bool,
    pub dcutr: bool,
}

impl Roles {
    #[must_use]
    pub const fn client() -> Self {
        Self {
            autonat_client: true,
            autonat_server: false,
            relay_client: true,
            relay_server: false,
            dcutr: true,
        }
    }

    #[must_use]
    pub const fn infrastructure() -> Self {
        Self {
            autonat_client: false,
            autonat_server: true,
            relay_client: false,
            relay_server: true,
            dcutr: false,
        }
    }

    #[must_use]
    pub const fn bare() -> Self {
        Self {
            autonat_client: false,
            autonat_server: false,
            relay_client: false,
            relay_server: false,
            dcutr: false,
        }
    }
}

#[derive(NetworkBehaviour)]
pub struct SpikeBehaviour {
    /// FIRST, so every dial meets it before the transport does.
    pub gate: InstrumentedGate,
    pub identify: identify::Behaviour,
    pub autonat_client:
        libp2p::swarm::behaviour::toggle::Toggle<Attributing<libp2p::autonat::v2::client::Behaviour>>,
    pub autonat_server:
        libp2p::swarm::behaviour::toggle::Toggle<Attributing<libp2p::autonat::v2::server::Behaviour>>,
    pub relay_client:
        libp2p::swarm::behaviour::toggle::Toggle<Attributing<relay::client::Behaviour>>,
    pub relay_server: libp2p::swarm::behaviour::toggle::Toggle<relay::Behaviour>,
    pub dcutr: libp2p::swarm::behaviour::toggle::Toggle<Attributing<libp2p::dcutr::Behaviour>>,
}

/// A built node and the instruments attached to it.
pub struct Node {
    pub swarm: Swarm<SpikeBehaviour>,
    /// The peers this node treats as its relays, for classifying a
    /// relay-client dial as a reservation rather than a circuit.
    pub relays: Arc<Mutex<BTreeSet<PeerId>>>,
    pub observed: crate::topology::Observed,
    pub peer_id: PeerId,
    pub identity: TransportIdentity,
    pub ledger: DialLedger,
    pub attribution: Attribution,
    pub manager: Arc<Mutex<ConnectionManager>>,
}

impl Node {
    /// Build a node whose trust sets are exactly what is passed.
    ///
    /// `data_plane` and `infrastructure` are separate arguments because
    /// ADR-0036 makes them separate TYPES: the whole question this
    /// spike asks about class is whether a peer authorized only for
    /// reachability stays out of the data plane, and a single list
    /// could not express it.
    pub fn new(
        roles: Roles,
        data_plane: &[TransportIdentity],
        infrastructure: &[TransportIdentity],
    ) -> Self {
        let keypair = identity::Keypair::generate_ed25519();
        let peer_id = PeerId::from_public_key(&keypair.public());
        let identity_str =
            TransportIdentity::parse(peer_id.to_base58()).expect("canonical PeerId");

        // CEILINGS SET EXPLICITLY. `ConnectionPolicy::default()`
        // carries `max_pending_dials: 0` and `max_connections: 0`, and
        // the manager enforces both — so a spike that took the default
        // would refuse every dial with `ConnectionLimitReached` and
        // could have been read as a policy finding rather than an
        // unconfigured harness. These are generous relative to what any
        // experiment here opens; the ceilings under test are the
        // per-class and per-origin ones, not the totals.
        let mut manager = ConnectionManager::new(
            ConnectionPolicy::new(SPIKE_MAX_PENDING_DIALS, SPIKE_MAX_CONNECTIONS),
            SPIKE_MAX_PENDING_DIALS,
        );
        let _ = manager.set_trust(
            TrustSources::new(
                PeerTrustPolicy::new(data_plane.iter().cloned()).expect("small allowlist"),
                InfrastructureSet::new(infrastructure.iter().cloned())
                    .expect("small infrastructure set"),
            ),
            &[],
        );
        let handle = manager.handle();
        let manager = Arc::new(Mutex::new(manager));

        let attribution = Attribution::default();
        // WHICH PEERS ARE RELAYS is configuration, not something a dial
        // reveals: `DialOpts::get_addresses` is `pub(crate)`, so the
        // `/p2p-circuit` suffix is invisible at announcement time. The
        // node records what it was told, exactly as production would
        // record its configured relays.
        let relays: Arc<Mutex<BTreeSet<PeerId>>> = Arc::new(Mutex::new(BTreeSet::new()));
        let gate = InstrumentedGate::new(handle, Arc::clone(&manager), attribution.clone());
        let ledger = gate.ledger();

        // THE BUILDER OWNS THE RELAY TRANSPORT. `with_relay_client`
        // installs the client transport into the stack and hands the
        // paired `client::Behaviour` to `with_behaviour`; constructing
        // our own with `relay::client::new` and dropping its
        // `Transport` panics the behaviour on its next poll
        // ("polled after channel from `Transport` has been closed"),
        // which is how this was found. The two halves are one object.
        let swarm = libp2p::SwarmBuilder::with_existing_identity(keypair)
            .with_tokio()
            .with_tcp(
                tcp::Config::default().nodelay(true),
                noise::Config::new,
                yamux::Config::default,
            )
            .expect("tcp")
            .with_relay_client(noise::Config::new, yamux::Config::default)
            .expect("relay client transport")
            .with_behaviour(|key, relay_client| SpikeBehaviour {
                gate,
                identify: identify::Behaviour::new(identify::Config::new(
                    "/interweave-spike/1.0.0".to_owned(),
                    key.public(),
                )),
                autonat_client: roles
                    .autonat_client
                    .then(|| {
                        Attributing::new(
                            // `Default` is the OsRng constructor the
                            // crate exposes without taking a `rand`
                            // dependency of our own; R1 records the
                            // probe interval it carries rather than
                            // assuming one.
                            libp2p::autonat::v2::client::Behaviour::default(),
                            always(DialOrigin::AutonatProbe),
                            attribution.clone(),
                        )
                    })
                    .into(),
                autonat_server: roles
                    .autonat_server
                    .then(|| {
                        Attributing::new(
                            libp2p::autonat::v2::server::Behaviour::default(),
                            always(DialOrigin::AutonatProbe),
                            attribution.clone(),
                        )
                    })
                    .into(),
                relay_client: roles
                    .relay_client
                    .then(|| {
                        Attributing::new(
                            relay_client,
                            relay_classifier(Arc::clone(&relays)),
                            attribution.clone(),
                        )
                    })
                    .into(),
                relay_server: roles
                    .relay_server
                    .then(|| relay::Behaviour::new(peer_id, relay::Config::default()))
                    .into(),
                dcutr: roles
                    .dcutr
                    .then(|| {
                        Attributing::new(
                            libp2p::dcutr::Behaviour::new(peer_id),
                            always(DialOrigin::DcutrHolePunch),
                            attribution.clone(),
                        )
                    })
                    .into(),
            })
            .expect("behaviour")
            .with_swarm_config(|c| c.with_idle_connection_timeout(Duration::from_secs(30)))
            .build();


        Self {
            swarm,
            relays,
            observed: crate::topology::Observed::default(),
            peer_id,
            identity: identity_str,
            ledger,
            attribution,
            manager,
        }
    }

    /// Name a peer as one of this node's relays.
    ///
    /// A dial the relay client makes TO a configured relay is a
    /// reservation; one to any other peer is a circuit toward that
    /// peer. That is the only discriminator available at announcement
    /// time, and it is configuration rather than inspection.
    pub fn add_relay(&mut self, relay: PeerId) {
        self.relays
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(relay);
    }

    /// Replace this node's data-plane allowlist.
    ///
    /// For a fixture that must build a node before it knows whom to
    /// trust — every node mints its own keypair, so an identity cannot
    /// be named in an allowlist until its node exists.
    pub fn trust_data_plane(&mut self, peers: &[TransportIdentity]) {
        let _ = self
            .manager
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .set_trust(
                TrustSources::new(
                    PeerTrustPolicy::new(peers.iter().cloned()).expect("small allowlist"),
                    InfrastructureSet::default(),
                ),
                &[],
            );
    }

    /// Dial as the COMMAND PATH does.
    ///
    /// Production's `GatedSwarm::dial` admits the dial first and
    /// registers its `ConnectionId` in `AdmittedDials`, so the gate's
    /// pending hook recognises it as ticketed rather than
    /// behaviour-originated. The spike models the same ordering with
    /// the attribution map — the caller knows its own origin, which is
    /// the whole point of the mechanism — so a harness dial is
    /// `Manual` and is not counted as a behaviour dial by accident.
    ///
    /// Without this the gate refuses the harness's own dials with "no
    /// announced origin", which is correct fail-closed behaviour and is
    /// how the omission was found.
    ///
    /// The peer is NAMED, as production's does: `DialRequest::peer` is
    /// what the policy classifies, and an anonymous dial can only be
    /// judged on limits and drain. The gate refuses one it cannot
    /// classify, which is also how this signature was found.
    pub fn dial(
        &mut self,
        peer: PeerId,
        address: Multiaddr,
    ) -> Result<(), libp2p::swarm::DialError> {
        let opts = libp2p::swarm::dial_opts::DialOpts::peer_id(peer)
            .addresses(vec![address])
            .condition(libp2p::swarm::dial_opts::PeerCondition::Always)
            .build();
        self.attribution
            .announce(opts.connection_id(), DialOrigin::Manual, Some(peer));
        self.swarm.dial(opts)
    }

    /// Listen on loopback and return the bound address.
    pub async fn listen(&mut self) -> Multiaddr {
        let addr: Multiaddr = "/ip4/127.0.0.1/tcp/0".parse().expect("loopback");
        self.swarm.listen_on(addr).expect("listen accepted");
        loop {
            if let libp2p::swarm::SwarmEvent::NewListenAddr { address, .. } =
                futures::StreamExt::select_next_some(&mut self.swarm).await
            {
                return address;
            }
        }
    }
}
