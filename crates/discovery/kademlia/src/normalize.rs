// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! Result normalization (§10) and the targeted-lookup key encoding (§9.2).

use interweave_discovery_api::{CandidatePeer, MAX_ADDRESS_BYTES, MAX_ADDRESSES};
use interweave_transport_api::TransportIdentity;
use std::collections::BTreeSet;

/// The identity-multihash envelope of a libp2p Ed25519 public-key
/// protobuf: identity code, 36-byte length, then field 1 varint
/// `KeyType::Ed25519` and field 2 bytes of length 0x20. Every byte of a
/// `12D3KooW…` PeerId is fixed except the 32 key bytes that follow.
const ED25519_ENVELOPE: [u8; 6] = [0x00, 0x24, 0x08, 0x01, 0x12, 0x20];

/// The 32-byte lookup key for a targeted query: the target's Ed25519
/// public key, recovered from its PeerId string.
///
/// The port's `StartQuery` carries `[u8; 32]` — "the key space is the
/// identifier space" — and for InterWeave identities that space is the
/// Ed25519 key: the rest of a `12D3KooW…` PeerId is a constant envelope,
/// so the driver reconstructs the exact PeerId from these bytes and asks
/// the DHT for its true location. A `Qm…` identity is a bare digest with
/// no recoverable key; it returns `None` and the caller refuses the
/// lookup rather than querying a point that is not the peer's.
pub(crate) fn targeted_lookup_key(peer: &TransportIdentity) -> Option<[u8; 32]> {
    let mut bytes = [0_u8; 64];
    let len = bs58::decode(peer.as_str()).onto(&mut bytes[..]).ok()?;
    let decoded = &bytes[..len];
    if decoded.len() != 38 || decoded[..6] != ED25519_ENVELOPE {
        return None;
    }
    let mut key = [0_u8; 32];
    key.copy_from_slice(&decoded[6..38]);
    Some(key)
}

/// One query-result candidate, filtered into what this provider will
/// track, or `None` for the local peer.
///
/// §10's rules, in order: discard self; reject an address whose trailing
/// `/p2p/<id>` names a DIFFERENT peer (an inconsistent suffix is the
/// address lying about who answers there — the address is dropped, the
/// peer is kept); cap addresses at the global per-peer limit. Trust,
/// channel, role, and application metadata are not attached — the type
/// this returns cannot carry them.
pub(crate) fn normalized_addresses(
    candidate: &CandidatePeer,
    local: &TransportIdentity,
) -> Option<BTreeSet<String>> {
    if candidate.peer_id == *local {
        return None;
    }
    let addresses: BTreeSet<String> = candidate
        .addresses
        .iter()
        .filter(|a| {
            !a.is_empty()
                && a.len() <= MAX_ADDRESS_BYTES
                && suffix_consistent(a, &candidate.peer_id)
        })
        .take(MAX_ADDRESSES)
        .cloned()
        .collect();
    Some(addresses)
}

/// Whether a trailing `/p2p/<id>` component agrees with the peer it is
/// attached to.
///
/// Only the FINAL component is judged: a `/p2p/` earlier in the address
/// (a relay path's inner hop) is part of the opaque route, and parsing
/// deeper than the suffix would put a multiaddr grammar into a crate
/// that deliberately has none.
fn suffix_consistent(address: &str, peer: &TransportIdentity) -> bool {
    match address.rfind("/p2p/") {
        None => true,
        Some(idx) => {
            let tail = &address[idx + 5..];
            tail.contains('/') || tail == peer.as_str()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const P1: &str = "12D3KooWDpJ7As7BWAwRMfu1VU2WCqNjvq387JEYKDBj4kx6nXTN";
    const P2: &str = "12D3KooWK99VoVxNE7XzyBwXEzW7xhK7Gpv85r9F3V3fyKSUKPH5";

    fn peer(s: &str) -> TransportIdentity {
        TransportIdentity::parse(s).expect("valid identity")
    }

    fn candidate(peer_id: &TransportIdentity, addresses: &[&str]) -> CandidatePeer {
        CandidatePeer {
            peer_id: peer_id.clone(),
            addresses: addresses.iter().map(|a| (*a).to_owned()).collect(),
            source: "kademlia".to_owned(),
            observed_at: 1_000,
            expires_at: None,
            protocol_observations: BTreeSet::new(),
        }
    }

    #[test]
    fn the_key_is_the_embedded_ed25519_public_key() {
        // Built from bytes, so the expectation is the input: the id IS
        // the envelope plus these 32 bytes, and extraction must return
        // exactly them.
        let mut bytes = [0_u8; 38];
        bytes[..6].copy_from_slice(&ED25519_ENVELOPE);
        let pk: [u8; 32] = core::array::from_fn(|i| u8::try_from(i).expect("fits") + 1);
        bytes[6..].copy_from_slice(&pk);
        let id = peer(&bs58::encode(bytes).into_string());
        assert_eq!(targeted_lookup_key(&id), Some(pk));
    }

    #[test]
    fn a_qm_identity_has_no_lookup_key() {
        let mut bytes = [0_u8; 34];
        bytes[..2].copy_from_slice(&[0x12, 0x20]);
        let id = peer(&bs58::encode(bytes).into_string());
        assert_eq!(
            targeted_lookup_key(&id),
            None,
            "a bare digest has no recoverable key; the caller must refuse, \
             not query the wrong point"
        );
    }

    #[test]
    fn the_local_peer_normalizes_to_nothing() {
        let local = peer(P1);
        assert_eq!(
            normalized_addresses(&candidate(&local, &["/ip4/192.0.2.1/tcp/4001"]), &local),
            None,
            "self is discarded whole, not emitted with filtered addresses"
        );
    }

    #[test]
    fn an_inconsistent_suffix_rejects_the_address_not_the_peer() {
        let local = peer(P1);
        let subject = peer(P2);
        let lying = format!("/ip4/192.0.2.1/tcp/4001/p2p/{P1}");
        let honest = format!("/ip4/192.0.2.2/tcp/4001/p2p/{P2}");
        let got = normalized_addresses(
            &candidate(&subject, &[&lying, &honest, "/ip4/192.0.2.3/tcp/4001"]),
            &local,
        )
        .expect("not self");
        assert!(
            !got.contains(&lying),
            "a suffix naming another peer is dropped"
        );
        assert!(got.contains(&honest), "a consistent suffix is kept as-is");
        assert!(got.contains("/ip4/192.0.2.3/tcp/4001"), "no suffix is fine");
    }

    #[test]
    fn an_inner_p2p_component_is_opaque_route() {
        let local = peer(P1);
        let subject = peer(P2);
        let relayed = format!("/ip4/192.0.2.1/tcp/4001/p2p/{P1}/p2p-circuit");
        let got =
            normalized_addresses(&candidate(&subject, &[&relayed]), &local).expect("not self");
        assert!(
            got.contains(&relayed),
            "a /p2p/ that is not the final component is part of the route, not a claim"
        );
    }

    #[test]
    fn addresses_cap_at_the_global_limit() {
        let local = peer(P1);
        let subject = peer(P2);
        let many: Vec<String> = (0..=MAX_ADDRESSES)
            .map(|i| format!("/ip4/198.51.100.{}/tcp/{}", i / 250, 1_000 + i))
            .collect();
        let refs: Vec<&str> = many.iter().map(String::as_str).collect();
        let got = normalized_addresses(&candidate(&subject, &refs), &local).expect("not self");
        assert_eq!(got.len(), MAX_ADDRESSES);
    }
}
