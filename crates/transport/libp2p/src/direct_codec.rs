// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The `/interweave/direct/2.0.0` request-response codec.
//!
//! A thin layer over `transport-api`'s frozen framing, and thin on
//! purpose: the bytes are decided there and pinned by
//! `fixtures/direct-v2/`, so this module's whole job is moving them
//! across a substream with a bound on how many it will read.
//!
//! # Every read is bounded before it allocates
//!
//! `read_to_end` on a peer-controlled substream is an unbounded
//! allocation with extra steps. Each direction here takes a
//! `.take(limit)` first, so a peer that opens a substream and streams
//! forever is cut off at a length this profile chose rather than at one
//! it runs out of memory discovering.
//!
//! The request limit is the frame ceiling: the largest legal
//! `DirectMessageV2` is the payload ceiling plus the fixed prefix, both
//! endpoint labels, and a media type — every one already bounded by its
//! own type. A frame larger than that cannot be legal, so refusing it at
//! the read costs nothing a legal sender would notice.
//!
//! # The response is not the request
//!
//! Responses are tiny and fixed-shape, so they get their own much
//! smaller bound. Reusing the request's ceiling would let a hostile
//! RESPONDER send a 48 KiB answer to a client that asked for a routing
//! decision, which is a resource asymmetry with no legitimate use.

use std::io;

use async_trait::async_trait;
use futures::{AsyncReadExt as _, AsyncWriteExt as _};
use libp2p::StreamProtocol;
use libp2p::request_response::Codec;

use interweave_transport_api::{
    DirectMessageV2, DirectRejectReason, EndpointId, MAX_MEDIA_TYPE_BYTES, MAX_PAYLOAD_BYTES,
    MessageId,
};

/// One inbound request, which may be a frame this node could not parse.
///
/// # A malformed frame is ANSWERED, not dropped
///
/// Mapping a decode failure to an `io::Error` makes request-response
/// emit `InboundFailure` and no `Request` at all, so the handler has
/// nothing to answer with and the peer observes a broken exchange. The
/// contract says otherwise: an over-ceiling payload is `too_large` and a
/// bad field is `malformed`, and `FrameError::to_wire` exists to say
/// which. Before this type, that function was reachable only from its
/// own unit tests.
///
/// `message_id` is the first sixteen bytes of the frame, so it survives
/// every failure that happens after them — which is every failure the
/// contract names a code for. A frame too short to carry one cannot be
/// answered at all, because a response must echo the id it answers; that
/// case stays an `io::Error` and is dropped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InboundRequest {
    /// A frame that decoded.
    Frame(Box<DirectMessageV2>),
    /// A frame that did not, and the code its sender is owed.
    Unparsable {
        /// Recovered from the first sixteen bytes.
        message_id: MessageId,
        /// From [`interweave_transport_api::FrameError::to_wire`].
        reason: DirectRejectReason,
    },
}

/// The protocol this codec speaks (ADR-0030).
///
/// Versioned in the string. A future incompatible framing is a NEW
/// protocol id, never a reinterpretation of this one — which is what
/// makes `UnsupportedProtocols` a usable major-version signal rather
/// than an ambiguous negotiation failure (SPIKE-002 finding 3).
pub const DIRECT_PROTOCOL: StreamProtocol = StreamProtocol::new("/interweave/direct/2.0.0");

/// Largest legal request frame, in bytes.
///
/// Every term is a ceiling its own type already enforces, summed rather
/// than guessed: a round number here would either refuse legal frames or
/// admit bytes no legal frame can contain.
pub const MAX_REQUEST_BYTES: usize = 16          // message_id
    + 8                                          // sent_at_ms
    + 1 + EndpointId::MAX_BYTES                   // source
    + 1 + EndpointId::MAX_BYTES                   // destination
    + 1 + MAX_MEDIA_TYPE_BYTES                    // media type
    + 4 + MAX_PAYLOAD_BYTES; // payload

/// Largest response, in bytes.
///
/// A response is a tag, a message id, and either an endpoint label or a
/// reason byte. Generous for that and nowhere near the request ceiling,
/// because a responder has no legitimate reason to send more.
pub const MAX_RESPONSE_BYTES: usize = 1 + 16 + 1 + EndpointId::MAX_BYTES;

/// The answer to a direct request.
///
/// Modelled here rather than in `transport-api` because the tag byte is a
/// codec concern: the neutral crate holds `AcceptedV2` and `RejectedV2`
/// as separate shapes, and how a wire distinguishes them is this layer's
/// decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DirectResponse {
    /// The resolved endpoint's queue accepted it.
    Accepted {
        /// Echoes the request id.
        message_id: MessageId,
        /// Which endpoint took it.
        resolved_endpoint: EndpointId,
    },
    /// It was refused, coarsely.
    Rejected {
        /// Echoes the request id.
        message_id: MessageId,
        /// The coarse reason.
        reason: DirectRejectReason,
    },
}

const TAG_ACCEPTED: u8 = 1;
const TAG_REJECTED: u8 = 2;

/// Wire codes for the coarse reasons.
///
/// Written out rather than derived from enum order: a reordering of
/// [`DirectRejectReason`] must not silently renumber the wire, and a new
/// variant must not accidentally take an existing code.
const fn reason_code(reason: DirectRejectReason) -> u8 {
    match reason {
        DirectRejectReason::NoRoute => 1,
        DirectRejectReason::UnauthorizedPeer => 2,
        DirectRejectReason::Overloaded => 3,
        DirectRejectReason::Malformed => 4,
        DirectRejectReason::TooLarge => 5,
        DirectRejectReason::ShuttingDown => 6,
        DirectRejectReason::Unsupported => 7,
    }
}

/// The inverse. An unknown code is `Unsupported` rather than an error:
/// a future peer may name a reason this build does not know, and the
/// honest local reading of that is "it refused, for a reason I cannot
/// interpret".
const fn reason_from_code(code: u8) -> Option<DirectRejectReason> {
    match code {
        1 => Some(DirectRejectReason::NoRoute),
        2 => Some(DirectRejectReason::UnauthorizedPeer),
        3 => Some(DirectRejectReason::Overloaded),
        4 => Some(DirectRejectReason::Malformed),
        5 => Some(DirectRejectReason::TooLarge),
        6 => Some(DirectRejectReason::ShuttingDown),
        7 => Some(DirectRejectReason::Unsupported),
        _ => None,
    }
}

/// The codec itself. Stateless.
#[derive(Debug, Clone, Default)]
pub struct DirectCodec;

/// Read at most `limit` bytes, then insist the substream had ended.
///
/// The `.take()` is the bound; the extra byte is how a frame that
/// EXCEEDED the limit is told from one that merely reached it. Without
/// that distinction an over-long frame would silently decode as a
/// truncated legal one.
async fn read_bounded<T>(io: &mut T, limit: usize) -> io::Result<Vec<u8>>
where
    T: futures::AsyncRead + Unpin + Send,
{
    let (buffer, oversize) = read_bounded_or_oversize(io, limit).await?;
    if oversize {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("frame exceeds the {limit}-byte ceiling"),
        ));
    }
    Ok(buffer)
}

/// Read at most `limit + 1` bytes, and SAY whether the limit was passed
/// rather than deciding what that means.
///
/// The extra byte is what distinguishes "exactly at the limit" from
/// "over it", and returning the buffer either way is what lets a request
/// that is too large still be ANSWERED: the bytes already read include
/// the message id, so there is something to echo. Erroring here instead
/// threw those bytes away and left the peer with a broken exchange for
/// the one failure mode the contract names `too_large` outright.
async fn read_bounded_or_oversize<T>(io: &mut T, limit: usize) -> io::Result<(Vec<u8>, bool)>
where
    T: futures::AsyncRead + Unpin + Send,
{
    let mut buffer = Vec::new();
    io.take(limit as u64 + 1).read_to_end(&mut buffer).await?;
    let oversize = buffer.len() > limit;
    Ok((buffer, oversize))
}

/// The rejection an over-ceiling frame is owed.
///
/// Separate from the decode path because it runs BEFORE decoding: there
/// is no `FrameError` to map, only a length that was already too big.
fn oversize_request(bytes: &[u8]) -> InboundRequest {
    // A frame past the request ceiling always carries sixteen bytes, so
    // the fallback is unreachable there — written as a rejection rather
    // than a panic because the lint policy treats a reachable panic in a
    // transport daemon as a defect and this function is public to the
    // crate.
    let message_id = recover_id(bytes).unwrap_or(MessageId::from_bytes([0; MessageId::LEN]));
    InboundRequest::Unparsable {
        message_id,
        reason: DirectRejectReason::TooLarge,
    }
}

/// The message id, when enough of the frame arrived to carry one.
///
/// It is the FIRST field, so it survives every failure occurring after
/// it — which is every failure the contract names a code for.
fn recover_id(bytes: &[u8]) -> Option<MessageId> {
    let head = bytes.get(..MessageId::LEN)?;
    let mut id = [0u8; MessageId::LEN];
    id.copy_from_slice(head);
    Some(MessageId::from_bytes(id))
}

#[async_trait]
impl Codec for DirectCodec {
    type Protocol = StreamProtocol;
    type Request = InboundRequest;
    type Response = DirectResponse;

    async fn read_request<T>(&mut self, _: &StreamProtocol, io: &mut T) -> io::Result<Self::Request>
    where
        T: futures::AsyncRead + Unpin + Send,
    {
        let (bytes, oversize) = read_bounded_or_oversize(io, MAX_REQUEST_BYTES).await?;
        if oversize {
            // TOO BIG TO DECODE, BIG ENOUGH TO ANSWER. The frame passed
            // the ceiling, so nothing here will parse it — but the bytes
            // already read start with the message id, which is all a
            // rejection needs. This is the failure mode `too_large`
            // exists for, and it used to be the one that got no answer.
            return Ok(oversize_request(&bytes));
        }
        match DirectMessageV2::decode(&bytes, MAX_PAYLOAD_BYTES) {
            Ok(frame) => Ok(InboundRequest::Frame(Box::new(frame))),
            Err(error) => {
                // THE ID COMES FIRST IN THE FRAME, so it outlives every
                // later field's failure. Without it there is nothing to
                // echo and therefore nothing that can be sent back.
                let Some(message_id) = recover_id(&bytes) else {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        error.to_string(),
                    ));
                };
                Ok(InboundRequest::Unparsable {
                    message_id,
                    reason: error.to_wire(),
                })
            }
        }
    }

    async fn read_response<T>(
        &mut self,
        _: &StreamProtocol,
        io: &mut T,
    ) -> io::Result<Self::Response>
    where
        T: futures::AsyncRead + Unpin + Send,
    {
        let bytes = read_bounded(io, MAX_RESPONSE_BYTES).await?;
        decode_response(&bytes).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
    }

    async fn write_request<T>(
        &mut self,
        _: &StreamProtocol,
        io: &mut T,
        request: Self::Request,
    ) -> io::Result<()>
    where
        T: futures::AsyncWrite + Unpin + Send,
    {
        // ONLY A FRAME IS SENDABLE. `Unparsable` describes something
        // that arrived, not something to send, and the send path
        // constructs it nowhere. Reported rather than panicked: the lint
        // policy here treats a reachable panic in a transport daemon as
        // a defect, and this is reachable by a caller mistake.
        let InboundRequest::Frame(frame) = request else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "a rejection is not a sendable request",
            ));
        };
        io.write_all(&frame.encode()).await?;
        io.close().await
    }

    async fn write_response<T>(
        &mut self,
        _: &StreamProtocol,
        io: &mut T,
        response: Self::Response,
    ) -> io::Result<()>
    where
        T: futures::AsyncWrite + Unpin + Send,
    {
        io.write_all(&encode_response(&response)).await?;
        io.close().await
    }
}

/// Encode a response.
#[must_use]
pub fn encode_response(response: &DirectResponse) -> Vec<u8> {
    let mut out = Vec::with_capacity(MAX_RESPONSE_BYTES);
    match response {
        DirectResponse::Accepted {
            message_id,
            resolved_endpoint,
        } => {
            out.push(TAG_ACCEPTED);
            out.extend_from_slice(message_id.as_bytes());
            let label = resolved_endpoint.as_str().as_bytes();
            out.push(u8::try_from(label.len()).unwrap_or(u8::MAX));
            out.extend_from_slice(label);
        }
        DirectResponse::Rejected { message_id, reason } => {
            out.push(TAG_REJECTED);
            out.extend_from_slice(message_id.as_bytes());
            out.push(reason_code(*reason));
        }
    }
    out
}

/// Decode a response.
///
/// # Errors
/// A string naming what was wrong. Local only: a sender that cannot read
/// a response has a `ProtocolViolation`, and the peer is told nothing.
pub fn decode_response(bytes: &[u8]) -> Result<DirectResponse, String> {
    let (&tag, rest) = bytes.split_first().ok_or("an empty response")?;
    if rest.len() < 16 {
        return Err("a response without a complete message id".to_owned());
    }
    let (id_bytes, rest) = rest.split_at(16);
    let mut id = [0u8; 16];
    id.copy_from_slice(id_bytes);
    let message_id = MessageId::from_bytes(id);

    match tag {
        TAG_ACCEPTED => {
            let (&len, label) = rest.split_first().ok_or("an acceptance without a length")?;
            let len = usize::from(len);
            if len == 0 {
                // AcceptedV2 always names the endpoint that took it. A
                // zero length here is not "the default" -- the default
                // was already resolved, and this field IS the answer.
                //
                // `EndpointId::parse` would reject the empty string a few
                // lines below, so this branch exists for its MESSAGE
                // rather than for the refusal: "no resolved endpoint" is
                // a wire-shape problem an operator can act on, where "an
                // illegal endpoint label: empty" describes the symptom.
                // The test asserts the wording, which is what makes this
                // branch load-bearing rather than decorative.
                return Err("an acceptance with no resolved endpoint".to_owned());
            }
            if label.len() != len {
                return Err(format!(
                    "an acceptance declaring {len} label bytes and carrying {}",
                    label.len()
                ));
            }
            let text = core::str::from_utf8(label).map_err(|_| "a non-UTF-8 endpoint label")?;
            let resolved_endpoint =
                EndpointId::parse(text).map_err(|e| format!("an illegal endpoint label: {e}"))?;
            Ok(DirectResponse::Accepted {
                message_id,
                resolved_endpoint,
            })
        }
        TAG_REJECTED => {
            let (&code, extra) = rest.split_first().ok_or("a rejection without a reason")?;
            if !extra.is_empty() {
                return Err(format!("{} byte(s) after a rejection", extra.len()));
            }
            let reason = reason_from_code(code).unwrap_or(DirectRejectReason::Unsupported);
            Ok(DirectResponse::Rejected { message_id, reason })
        }
        other => Err(format!("an unknown response tag {other}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message_id() -> MessageId {
        MessageId::from_bytes([3; 16])
    }

    fn endpoint(name: &str) -> EndpointId {
        EndpointId::parse(name).expect("valid endpoint id")
    }

    #[test]
    fn an_acceptance_round_trips() {
        let response = DirectResponse::Accepted {
            message_id: message_id(),
            resolved_endpoint: endpoint("claude"),
        };
        assert_eq!(
            decode_response(&encode_response(&response)),
            Ok(response.clone())
        );
    }

    #[test]
    fn every_rejection_reason_round_trips() {
        for reason in [
            DirectRejectReason::NoRoute,
            DirectRejectReason::UnauthorizedPeer,
            DirectRejectReason::Overloaded,
            DirectRejectReason::Malformed,
            DirectRejectReason::TooLarge,
            DirectRejectReason::ShuttingDown,
            DirectRejectReason::Unsupported,
        ] {
            let response = DirectResponse::Rejected {
                message_id: message_id(),
                reason,
            };
            assert_eq!(
                decode_response(&encode_response(&response)),
                Ok(response),
                "{reason:?} did not survive the wire"
            );
        }
    }

    /// Codes are assigned, not derived from declaration order. Reordering
    /// the enum must not renumber the wire.
    #[test]
    fn reason_codes_are_fixed_values() {
        assert_eq!(reason_code(DirectRejectReason::NoRoute), 1);
        assert_eq!(reason_code(DirectRejectReason::UnauthorizedPeer), 2);
        assert_eq!(reason_code(DirectRejectReason::Overloaded), 3);
        assert_eq!(reason_code(DirectRejectReason::Malformed), 4);
        assert_eq!(reason_code(DirectRejectReason::TooLarge), 5);
        assert_eq!(reason_code(DirectRejectReason::ShuttingDown), 6);
        assert_eq!(reason_code(DirectRejectReason::Unsupported), 7);
    }

    /// A reason this build does not know is a refusal it cannot
    /// interpret, not a decode failure. A future peer must be able to
    /// refuse us for a new reason without breaking the exchange.
    #[test]
    fn an_unknown_reason_code_reads_as_unsupported() {
        let mut bytes = encode_response(&DirectResponse::Rejected {
            message_id: message_id(),
            reason: DirectRejectReason::NoRoute,
        });
        *bytes.last_mut().expect("a reason byte") = 200;
        assert_eq!(
            decode_response(&bytes),
            Ok(DirectResponse::Rejected {
                message_id: message_id(),
                reason: DirectRejectReason::Unsupported
            })
        );
    }

    #[test]
    fn a_truncated_response_is_refused_at_every_boundary() {
        let bytes = encode_response(&DirectResponse::Accepted {
            message_id: message_id(),
            resolved_endpoint: endpoint("claude"),
        });
        for cut in 0..bytes.len() {
            assert!(
                decode_response(&bytes[..cut]).is_err(),
                "a response truncated to {cut} bytes must not decode"
            );
        }
        assert!(decode_response(&bytes).is_ok());
    }

    #[test]
    fn an_acceptance_must_name_an_endpoint() {
        let mut bytes = encode_response(&DirectResponse::Accepted {
            message_id: message_id(),
            resolved_endpoint: endpoint("claude"),
        });
        bytes.truncate(1 + 16);
        bytes.push(0);
        let error = decode_response(&bytes).expect_err("a zero-length label is refused");
        assert!(
            error.contains("no resolved endpoint"),
            "refused as a missing ANSWER, not as a malformed label: {error}"
        );
    }

    #[test]
    fn trailing_bytes_after_a_rejection_are_refused() {
        let mut bytes = encode_response(&DirectResponse::Rejected {
            message_id: message_id(),
            reason: DirectRejectReason::NoRoute,
        });
        bytes.push(0);
        assert!(decode_response(&bytes).is_err());
    }

    #[test]
    fn an_unknown_tag_is_refused() {
        let mut bytes = encode_response(&DirectResponse::Rejected {
            message_id: message_id(),
            reason: DirectRejectReason::NoRoute,
        });
        bytes[0] = 99;
        assert!(decode_response(&bytes).is_err());
    }

    /// The request ceiling is the sum of ceilings its own types enforce,
    /// so a legal maximum frame fits exactly and nothing larger can.
    #[test]
    fn the_request_ceiling_admits_the_largest_legal_frame() {
        use interweave_transport_api::{MediaType, Payload};
        let largest = DirectMessageV2 {
            message_id: message_id(),
            sent_at_ms: u64::MAX,
            source_endpoint: endpoint(&"a".repeat(EndpointId::MAX_BYTES)),
            destination_endpoint: Some(endpoint(&"b".repeat(EndpointId::MAX_BYTES))),
            payload: Payload::at_ceiling(
                Some(MediaType::parse("x".repeat(MAX_MEDIA_TYPE_BYTES)).expect("valid media type")),
                vec![0; MAX_PAYLOAD_BYTES],
            )
            .expect("at the ceiling"),
        };
        assert_eq!(
            largest.encode().len(),
            MAX_REQUEST_BYTES,
            "the ceiling is exactly the largest legal frame"
        );
    }

    /// A malformed frame becomes an ANSWERABLE rejection, not an error.
    ///
    /// The defect this replaces: mapping the decode failure to an
    /// `io::Error` made request-response emit `InboundFailure` and no
    /// `Request`, so the handler had nothing to answer with and the peer
    /// saw a broken exchange instead of the code it is owed.
    #[tokio::test]
    async fn an_over_ceiling_payload_reads_back_as_too_large() {
        let mut frame = DirectMessageV2 {
            message_id: message_id(),
            sent_at_ms: 1,
            source_endpoint: endpoint("human"),
            destination_endpoint: Some(endpoint("claude")),
            payload: interweave_transport_api::Payload::at_ceiling(None, b"hi".to_vec())
                .expect("within the ceiling"),
        }
        .encode();
        // Overstate the declared payload length, which is the last four
        // bytes before the payload itself.
        let at = frame.len() - 4 - 2;
        frame[at..at + 4].copy_from_slice(&u32::MAX.to_be_bytes());

        let read = read_one_request(&frame).await.expect("answerable");
        assert_eq!(
            read,
            InboundRequest::Unparsable {
                message_id: message_id(),
                reason: DirectRejectReason::TooLarge,
            },
            "the sender is told its payload was too large"
        );
    }

    #[tokio::test]
    async fn a_bad_field_reads_back_as_malformed() {
        let mut frame = DirectMessageV2 {
            message_id: message_id(),
            sent_at_ms: 1,
            source_endpoint: endpoint("human"),
            destination_endpoint: None,
            payload: interweave_transport_api::Payload::at_ceiling(None, b"hi".to_vec())
                .expect("within the ceiling"),
        }
        .encode();
        // Claim a source endpoint longer than the bytes that follow.
        frame[MessageId::LEN + 8] = 200;

        let read = read_one_request(&frame).await.expect("answerable");
        assert_eq!(
            read,
            InboundRequest::Unparsable {
                message_id: message_id(),
                reason: DirectRejectReason::Malformed,
            },
            "and a bad field is malformed, not too_large"
        );
    }

    /// TOO SHORT TO ANSWER. A response must echo the id it answers, so a
    /// frame that does not even carry one cannot be refused on the wire
    /// — it stays an I/O error and the exchange is dropped.
    #[tokio::test]
    async fn a_frame_too_short_to_carry_an_id_stays_an_error() {
        let truncated = vec![0u8; MessageId::LEN - 1];
        assert!(
            read_one_request(&truncated).await.is_err(),
            "nothing to echo, so nothing to send"
        );
    }

    /// A rejection is not something this node sends as a REQUEST.
    ///
    /// Reported rather than panicked: the lint policy treats a reachable
    /// panic in a transport daemon as a defect, and a caller mistake is
    /// reachable.
    #[tokio::test]
    async fn writing_a_rejection_as_a_request_is_refused() {
        let mut sink: Vec<u8> = Vec::new();
        let written = DirectCodec
            .write_request(
                &DIRECT_PROTOCOL,
                &mut futures::io::Cursor::new(&mut sink),
                InboundRequest::Unparsable {
                    message_id: message_id(),
                    reason: DirectRejectReason::Malformed,
                },
            )
            .await;
        assert!(written.is_err(), "not a sendable request");
        assert!(sink.is_empty(), "and nothing was written");
    }

    /// Run `read_request` over a byte slice.
    async fn read_one_request(bytes: &[u8]) -> io::Result<InboundRequest> {
        DirectCodec
            .read_request(&DIRECT_PROTOCOL, &mut futures::io::Cursor::new(bytes))
            .await
    }
}
