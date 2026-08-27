// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! `GossipSubMessageIdV1`, the frozen mesh duplicate identity.
//!
//! From `architecture/transport/libp2p/PUBSUB.md`, frozen by four vectors
//! in `fixtures/gossipsub/gossipsub-message-id-v1.json`.
//!
//! ```text
//! domain    = "interweave/gossipsub-message-id/v1\0"
//! canonical = domain || u16be(len(source)) || source || u64be(sequence)
//! id        = SHA-256(canonical)
//! ```
//!
//! # It binds transport metadata and nothing else
//!
//! `source` is the authenticated GossipSub source PeerId's raw bytes and
//! `sequence` is the wire sequence number. PUBSUB.md makes it a MUST that
//! the application envelope's `message_id` is **not** an input: two
//! publishers may legitimately choose the same 128 bits, and a mesh that
//! keyed on that field would drop a message nobody sent twice.
//!
//! This function therefore takes bytes and a number rather than an
//! envelope — it cannot read one, so it cannot come to depend on
//! application serialization.
//!
//! # Why bytes rather than a PeerId
//!
//! The neutral contract keeps `TransportIdentity` a validated string and
//! offers no byte accessor: "backends that need the bytes parse it
//! themselves". A backend passes `PeerId::to_bytes()` here, and this
//! crate stays free of a libp2p type.
//!
//! # Changing this is a compatibility decision
//!
//! PUBSUB.md: it is network-compatibility behaviour, not local cache
//! tuning. A peer computing a different id sees different duplicates.

use sha2::{Digest, Sha256};

/// The domain prefix, ASCII followed by one `0x00`.
pub const DOMAIN: &[u8] = b"interweave/gossipsub-message-id/v1\x00";

/// The 32-byte mesh duplicate identity.
///
/// A newtype for the same reason [`crate::topic::TopicKey`] is one: both
/// are 32-byte SHA-256 values derived in this crate, and swapping them
/// would compile and be wrong.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MeshMessageId([u8; 32]);

impl MeshMessageId {
    /// The raw digest, which is what the backend keys its cache on.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// Derive the mesh identity from the authenticated source and sequence.
///
/// `source` is `PeerId::to_bytes()` — the canonical raw multihash, not a
/// printable form. The length prefix is `u16be` and covers exactly those
/// bytes, so a longer identity in some future multihash cannot be
/// confused with a shorter one followed by the sequence number.
#[must_use]
pub fn gossipsub_message_id_v1(source: &[u8], sequence_number: u64) -> MeshMessageId {
    let mut hasher = Sha256::new();
    hasher.update(DOMAIN);
    // LENGTH-PREFIXED, and this is the field order the golden pins: the
    // prefix precedes the bytes it measures. Writing the source first
    // and its length after would hash the same bytes in a different
    // arrangement and produce a plausible wrong id.
    hasher.update(
        u16::try_from(source.len())
            .unwrap_or(u16::MAX)
            .to_be_bytes(),
    );
    hasher.update(source);
    hasher.update(sequence_number.to_be_bytes());
    MeshMessageId(hasher.finalize().into())
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &[u8] = &[0x00, 0x24, 0x08, 0x01, 0x12, 0x20, 0xaa];
    const B: &[u8] = &[0x00, 0x24, 0x08, 0x01, 0x12, 0x20, 0xbb];

    #[test]
    fn the_domain_is_terminated() {
        assert_eq!(DOMAIN.last(), Some(&0));
        assert_eq!(
            &DOMAIN[..DOMAIN.len() - 1],
            b"interweave/gossipsub-message-id/v1"
        );
    }

    #[test]
    fn two_publishers_at_one_sequence_number_do_not_collide() {
        // The property the whole function exists for. Keying on anything
        // the two share -- an envelope id, a sequence number alone --
        // would let one publisher suppress the other's message.
        assert_ne!(gossipsub_message_id_v1(A, 0), gossipsub_message_id_v1(B, 0));
    }

    #[test]
    fn one_publisher_at_two_sequence_numbers_does_not_collide() {
        assert_ne!(gossipsub_message_id_v1(A, 0), gossipsub_message_id_v1(A, 1));
        // Including the far edge of the u64 the wire carries.
        assert_ne!(
            gossipsub_message_id_v1(A, u64::MAX),
            gossipsub_message_id_v1(A, u64::MAX - 1)
        );
    }

    #[test]
    fn the_length_prefix_stops_a_source_and_sequence_from_being_reparsed() {
        // Without the u16be length, a source whose trailing bytes happen
        // to look like the start of another source would let two
        // different (source, sequence) pairs canonicalise identically.
        // This is that ambiguity made concrete: same concatenated bytes,
        // different split.
        let long = [A, &[0u8; 8]].concat();
        assert_ne!(
            gossipsub_message_id_v1(&long, 0),
            gossipsub_message_id_v1(A, 0),
            "the length must distinguish where the source ends"
        );
    }

    #[test]
    fn the_id_is_deterministic() {
        assert_eq!(
            gossipsub_message_id_v1(A, 42),
            gossipsub_message_id_v1(A, 42)
        );
    }
}
