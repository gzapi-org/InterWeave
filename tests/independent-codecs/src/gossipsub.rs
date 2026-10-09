// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton

//! The two GossipSub derivations `transport/libp2p/PUBSUB.md` freezes:
//!
//! ```text
//! TopicKeyV1           = SHA-256("interweave/topic/v1\0" || channel_id_ascii)
//! GossipSubMessageIdV1 = SHA-256("interweave/gossipsub-message-id/v1\0"
//!                                || u16be(len(source)) || source || u64be(sequence_number))
//!                        where source = PeerId::to_bytes()
//! ```
//!
//! The topic KEY is fixed by the text. The topic's WIRE STRING, the
//! encoding peers subscribe by, is left to the implementation at the time
//! of writing (PUBSUB.md line 11). That gap is raised with architect-cto
//! (01a11e67), so no wire string is produced here until the encoding is
//! ruled.
//!
//! `PeerId::to_bytes()` is the multihash the `PeerId`'s base58btc text
//! spells, so the source bytes are that text decoded. The decoder is here
//! too, so no identity crate is needed.

use sha2::{Digest, Sha256};

use crate::DecodeError;

/// The topic domain, its terminating zero included.
pub const TOPIC_DOMAIN: &[u8] = b"interweave/topic/v1\0";

/// The message-id domain, its terminating zero included.
pub const MESSAGE_ID_DOMAIN: &[u8] = b"interweave/gossipsub-message-id/v1\0";

/// ADR-0025: `[A-Za-z0-9][A-Za-z0-9._:/-]{0,127}`, case-sensitive.
#[must_use]
pub fn is_channel_id(s: &str) -> bool {
    let b = s.as_bytes();
    match b.split_first() {
        Some((first, rest)) => {
            first.is_ascii_alphanumeric()
                && rest.len() <= 127
                && rest.iter().all(|c| {
                    c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b':' | b'/' | b'-')
                })
        }
        None => false,
    }
}

/// The topic key for a channel.
///
/// # Errors
/// A channel outside the `ChannelId` grammar.
pub fn topic_key_v1(channel: &str) -> Result<[u8; 32], DecodeError> {
    if !is_channel_id(channel) {
        return Err(DecodeError(format!(
            "{channel:?} is outside the ChannelId grammar"
        )));
    }
    let mut h = Sha256::new();
    h.update(TOPIC_DOMAIN);
    h.update(channel.as_bytes());
    Ok(h.finalize().into())
}

/// The mesh message id for a source `PeerId` (its text) and a sequence
/// number.
///
/// # Errors
/// A source that is not base58btc, or longer than a u16 can count.
pub fn message_id_v1(source_peer_id: &str, sequence_number: u64) -> Result<[u8; 32], DecodeError> {
    let source = base58btc_decode(source_peer_id)?;
    let len = u16::try_from(source.len())
        .map_err(|_| DecodeError("a source longer than u16".to_owned()))?;
    let mut h = Sha256::new();
    h.update(MESSAGE_ID_DOMAIN);
    h.update(len.to_be_bytes());
    h.update(&source);
    h.update(sequence_number.to_be_bytes());
    Ok(h.finalize().into())
}

const ALPHABET: &[u8; 58] = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";

/// Decode base58btc: each leading `1` is a leading zero byte, the rest a
/// big-endian base-58 number.
///
/// # Errors
/// A character outside the alphabet, or an empty string.
pub fn base58btc_decode(text: &str) -> Result<Vec<u8>, DecodeError> {
    if text.is_empty() {
        return Err(DecodeError("an empty base58 string".to_owned()));
    }
    let zeros = text.bytes().take_while(|&c| c == b'1').count();
    // Little-endian base-256 accumulator.
    let mut acc: Vec<u8> = Vec::new();
    for c in text.bytes() {
        let digit = ALPHABET
            .iter()
            .position(|&a| a == c)
            .ok_or_else(|| DecodeError(format!("{:?} is not base58btc", char::from(c))))?;
        let mut carry = u32::try_from(digit).unwrap_or(0);
        for byte in &mut acc {
            carry += u32::from(*byte) * 58;
            *byte = u8::try_from(carry & 0xff).unwrap_or(0);
            carry >>= 8;
        }
        while carry > 0 {
            acc.push(u8::try_from(carry & 0xff).unwrap_or(0));
            carry >>= 8;
        }
    }
    let mut out = vec![0u8; zeros];
    out.extend(acc.iter().rev());
    Ok(out)
}
