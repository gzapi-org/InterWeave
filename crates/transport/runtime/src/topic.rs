// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! `ChannelId` to GossipSub topic, the frozen derivation.
//!
//! From `architecture/transport/libp2p/PUBSUB.md`. A pure function over
//! the channel name and nothing else, frozen by five vectors in
//! `fixtures/gossipsub/gossipsub-topic-key-v1.json`.
//!
//! ```text
//! sha256("interweave/topic/v1\0" || channel_id_ascii)
//! ```
//!
//! # What the hash does and does not buy
//!
//! It keeps raw channel names off the wire, and PUBSUB.md is explicit
//! that it **does not resist dictionary guessing** of a low-entropy name.
//! A topic is not a secret and nothing may be built as though it were.
//!
//! # Validate before hashing
//!
//! ADR-0025 requires the ChannelId to satisfy its grammar before it is
//! hashed, which is why this takes a parsed [`ChannelId`] rather than a
//! string: an unvalidated name would produce a perfectly good-looking
//! topic that no conforming peer derives.

use sha2::{Digest, Sha256};

use interweave_transport_api::ChannelId;

/// The domain prefix, ASCII followed by one `0x00`.
///
/// The terminator is load-bearing for the same reason it is in the
/// content fingerprint: without it one domain could be a prefix of
/// another, and two different constructions could hash the same bytes.
pub const DOMAIN: &[u8] = b"interweave/topic/v1\x00";

/// The 32-byte topic key a channel maps to.
///
/// A newtype rather than a bare `[u8; 32]` because this crate now derives
/// two different 32-byte SHA-256 values — this and the mesh message id —
/// and they are not interchangeable. Handing one to a function expecting
/// the other would compile and produce a plausible wrong answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TopicKey([u8; 32]);

impl TopicKey {
    /// The form that goes on the wire as the GossipSub topic string.
    ///
    /// PUBSUB.md leaves the encoding to the implementation; this picks
    /// lower-case hex, and it is derived here rather than at each call
    /// site so two peers cannot disagree about the representation of a
    /// key they agree about.
    #[must_use]
    pub fn wire_string(&self) -> String {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut s = String::with_capacity(64);
        for b in self.0 {
            s.push(char::from(HEX[usize::from(b >> 4)]));
            s.push(char::from(HEX[usize::from(b & 0x0f)]));
        }
        s
    }
}

/// Derive the topic key for a channel.
#[must_use]
pub fn topic_key_v1(channel: &ChannelId) -> TopicKey {
    let mut hasher = Sha256::new();
    hasher.update(DOMAIN);
    hasher.update(channel.as_str().as_bytes());
    TopicKey(hasher.finalize().into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn channel(name: &str) -> ChannelId {
        ChannelId::parse(name).expect("valid channel id")
    }

    #[test]
    fn the_domain_is_terminated_so_one_prefix_cannot_become_another() {
        // Without the NUL a channel named `x` under domain `d` and a
        // channel named `` under domain `dx` would hash identically. The
        // terminator is what makes the domain unambiguous, so assert the
        // byte rather than trusting the literal.
        assert_eq!(DOMAIN.last(), Some(&0));
        assert_eq!(&DOMAIN[..DOMAIN.len() - 1], b"interweave/topic/v1");
    }

    #[test]
    fn channels_differing_only_in_case_are_different_topics() {
        // ADR-0025 makes ChannelId case-sensitive. A case-folding
        // "convenience" anywhere above this would silently merge two
        // channels into one mesh, which no error would ever report.
        assert_ne!(
            topic_key_v1(&channel("general")),
            topic_key_v1(&channel("General"))
        );
    }

    #[test]
    fn the_wire_string_is_lower_case_hex_of_the_whole_digest() {
        let key = topic_key_v1(&channel("general"));
        let wire = key.wire_string();
        assert_eq!(wire.len(), 64, "32 bytes, two characters each");
        assert!(
            wire.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
            "lower-case hex only: {wire}"
        );
        // That it is the digest, not a truncation or a re-hash, is proved
        // by `frozen_derivation_vectors`: `wire_string` there is compared
        // to the fixture's independently computed sha256 hex.
    }

    #[test]
    fn a_prefix_is_not_the_same_topic_as_what_it_prefixes() {
        // `a` and `ab` differ by a byte the hash must see; a derivation
        // that padded or truncated would collapse a family of channels.
        assert_ne!(topic_key_v1(&channel("a")), topic_key_v1(&channel("ab")));
    }
}
