// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The `/interweave/endpoints/1.0.0` request-response codec (ADR-0031).
//!
//! The bytes are decided in `architecture/transport/libp2p/ENDPOINTS.md`
//! and pinned by `fixtures/endpoints/endpoint-directory-v1-frame.json`;
//! this module moves them across a substream with a bound on how many
//! it will read, exactly as `direct_codec` does for direct v2.
//!
//! # Both reads are bounded before they allocate
//!
//! A request is ONE byte and a response is at most 2094, and each read
//! `.take()`s its ceiling plus one — the extra byte being how "exactly
//! at the limit" is told from "over it". A peer that streams forever is
//! cut off at a length this profile chose.
//!
//! # Grammar is refused here, not later
//!
//! An [`EndpointId`] cannot be constructed without passing its grammar,
//! so a response carrying a bad label fails to DECODE, and request-
//! response reports that to the requester as an `Io(InvalidData)` the
//! runtime reads as `ProtocolViolation`. The count and duplicate rules
//! are the runtime's (`transport-runtime::directory`) because they are
//! about the list, not the bytes — and the codec refuses a count above
//! 32 only because such a frame cannot fit the response ceiling.

use std::io;

use async_trait::async_trait;
use futures::{AsyncReadExt as _, AsyncWriteExt as _};
use libp2p::StreamProtocol;
use libp2p::request_response::Codec;

use interweave_transport_api::{
    DirectoryRefusal, EndpointDirectoryV1, EndpointId, MAX_DIRECTORY_ENTRIES,
};

/// The protocol this codec speaks (ADR-0031). Versioned in the string,
/// so an incompatible framing is a NEW id and `UnsupportedProtocols`
/// stays a usable major-version signal.
pub const ENDPOINTS_PROTOCOL: StreamProtocol = StreamProtocol::new("/interweave/endpoints/1.0.0");

/// The whole request: one tag byte. Not empty, because an empty request
/// is indistinguishable from a closed stream.
pub const MAX_REQUEST_BYTES: usize = 1;

/// The largest response: tag, `generated_at_ms`, `ttl_ms`, count, and
/// 32 entries each at the 64-byte label ceiling. 2094, and the spec
/// quotes that number from these same widths.
pub const MAX_RESPONSE_BYTES: usize =
    1 + 8 + 4 + 1 + MAX_DIRECTORY_ENTRIES * (1 + EndpointId::MAX_BYTES);

const TAG_REQUEST: u8 = 0x01;
const TAG_DIRECTORY: u8 = 0x01;
const TAG_REFUSED: u8 = 0x02;

/// The request. Carries nothing; the peer's authenticated identity is
/// the whole of the question.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ListEndpointsV1;

/// The answer: a directory, or a coarse refusal carrying no list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DirectoryResponse {
    /// The advertised endpoints, unvalidated on the way in.
    Directory(EndpointDirectoryV1),
    /// Refused; nothing about any endpoint is disclosed.
    Refused(DirectoryRefusal),
}

/// Wire codes, hand-assigned so a reordering of the enum cannot renumber
/// the wire.
const fn reason_code(reason: DirectoryRefusal) -> u8 {
    match reason {
        DirectoryRefusal::Overloaded => 1,
        DirectoryRefusal::Unauthorized => 2,
        DirectoryRefusal::Unavailable => 3,
    }
}

/// The inverse. An unknown code reads as `Unavailable`: the peer refused,
/// for a reason this build cannot name, and "not available to me" is the
/// honest local reading of that.
const fn reason_from_code(code: u8) -> DirectoryRefusal {
    match code {
        1 => DirectoryRefusal::Overloaded,
        2 => DirectoryRefusal::Unauthorized,
        _ => DirectoryRefusal::Unavailable,
    }
}

/// Encode the request.
#[must_use]
pub const fn encode_request() -> [u8; 1] {
    [TAG_REQUEST]
}

/// Decode a request.
///
/// # Errors
/// Anything but the single tag byte.
pub fn decode_request(bytes: &[u8]) -> Result<ListEndpointsV1, &'static str> {
    match bytes {
        [TAG_REQUEST] => Ok(ListEndpointsV1),
        [] => Err("an empty request is a closed stream, not a query"),
        _ => Err("a request is exactly one tag byte"),
    }
}

/// Encode a response, per the frozen layout.
#[must_use]
pub fn encode_response(response: &DirectoryResponse) -> Vec<u8> {
    match response {
        DirectoryResponse::Directory(directory) => {
            let mut out = Vec::with_capacity(MAX_RESPONSE_BYTES);
            out.push(TAG_DIRECTORY);
            out.extend_from_slice(&directory.generated_at_ms.to_be_bytes());
            out.extend_from_slice(&directory.ttl_ms.to_be_bytes());
            // A count above 32 cannot be encoded in a legal frame; the
            // responder never builds one (`advertised_for` caps at the
            // wire bound), so this truncation is unreachable and exists
            // so the encoder cannot produce bytes the decoder refuses.
            let entries = directory
                .endpoints
                .iter()
                .take(MAX_DIRECTORY_ENTRIES)
                .collect::<Vec<_>>();
            out.push(u8::try_from(entries.len()).unwrap_or(u8::MAX));
            for endpoint in entries {
                let label = endpoint.as_str().as_bytes();
                out.push(u8::try_from(label.len()).unwrap_or(u8::MAX));
                out.extend_from_slice(label);
            }
            out
        }
        DirectoryResponse::Refused(reason) => vec![TAG_REFUSED, reason_code(*reason)],
    }
}

/// Decode a response.
///
/// # Errors
/// A static description of the first thing wrong: unknown tag, short
/// frame, a label outside the EndpointId grammar, more than 32 entries,
/// or trailing bytes.
pub fn decode_response(bytes: &[u8]) -> Result<DirectoryResponse, &'static str> {
    let (&tag, rest) = bytes.split_first().ok_or("empty response")?;
    match tag {
        TAG_REFUSED => match rest {
            [code] => Ok(DirectoryResponse::Refused(reason_from_code(*code))),
            _ => Err("a refusal is a tag and one reason byte"),
        },
        TAG_DIRECTORY => {
            let (generated, rest) = rest.split_at_checked(8).ok_or("short generated_at_ms")?;
            let (ttl, rest) = rest.split_at_checked(4).ok_or("short ttl_ms")?;
            let (&count, mut rest) = rest.split_first().ok_or("short count")?;
            let count = usize::from(count);
            if count > MAX_DIRECTORY_ENTRIES {
                return Err("more entries than the frame may carry");
            }
            let mut endpoints = Vec::with_capacity(count);
            for _ in 0..count {
                let (&len, tail) = rest.split_first().ok_or("short entry length")?;
                let (label, tail) = tail
                    .split_at_checked(usize::from(len))
                    .ok_or("short entry")?;
                let label = std::str::from_utf8(label).map_err(|_| "entry is not ASCII")?;
                endpoints.push(EndpointId::parse(label).map_err(|_| "entry outside the grammar")?);
                rest = tail;
            }
            if !rest.is_empty() {
                return Err("trailing bytes after the last entry");
            }
            let generated_at_ms = u64::from_be_bytes(generated.try_into().map_err(|_| "width")?);
            let ttl_ms = u32::from_be_bytes(ttl.try_into().map_err(|_| "width")?);
            Ok(DirectoryResponse::Directory(EndpointDirectoryV1 {
                generated_at_ms,
                ttl_ms,
                endpoints,
            }))
        }
        _ => Err("unknown response tag"),
    }
}

/// Read at most `limit` bytes, refusing a stream that carries more.
async fn read_bounded<T>(io: &mut T, limit: usize) -> io::Result<Vec<u8>>
where
    T: futures::AsyncRead + Unpin + Send,
{
    let mut buffer = Vec::new();
    io.take(limit as u64 + 1).read_to_end(&mut buffer).await?;
    if buffer.len() > limit {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "frame exceeds the protocol ceiling",
        ));
    }
    Ok(buffer)
}

/// The codec. Stateless.
#[derive(Debug, Clone, Copy, Default)]
pub struct EndpointsCodec;

#[async_trait]
impl Codec for EndpointsCodec {
    type Protocol = StreamProtocol;
    type Request = ListEndpointsV1;
    type Response = DirectoryResponse;

    async fn read_request<T>(&mut self, _: &StreamProtocol, io: &mut T) -> io::Result<Self::Request>
    where
        T: futures::AsyncRead + Unpin + Send,
    {
        let bytes = read_bounded(io, MAX_REQUEST_BYTES).await?;
        decode_request(&bytes).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
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
        _: Self::Request,
    ) -> io::Result<()>
    where
        T: futures::AsyncWrite + Unpin + Send,
    {
        io.write_all(&encode_request()).await?;
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

#[cfg(test)]
mod tests {
    use super::*;

    fn ep(name: &str) -> EndpointId {
        EndpointId::parse(name).expect("valid")
    }

    fn unhex(text: &str) -> Vec<u8> {
        (0..text.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&text[i..i + 2], 16).expect("hex"))
            .collect()
    }

    /// The frozen frames, byte for byte. Read from the fixture file so
    /// this test cannot agree with the encoder for free.
    fn fixture() -> serde_json::Value {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../fixtures/endpoints/endpoint-directory-v1-frame.json");
        serde_json::from_str(&std::fs::read_to_string(path).expect("fixture present"))
            .expect("fixture parses")
    }

    #[test]
    fn every_frozen_frame_round_trips() {
        let doc = fixture();
        let mut seen = 0;
        for v in doc["vectors"].as_array().expect("vectors") {
            let frame = unhex(v["frame_hex"].as_str().expect("hex"));
            match v["kind"].as_str().expect("kind") {
                "request" => {
                    assert_eq!(encode_request().to_vec(), frame);
                    assert_eq!(decode_request(&frame), Ok(ListEndpointsV1));
                }
                "refused" => {
                    let decoded = decode_response(&frame).expect("decodes");
                    assert_eq!(encode_response(&decoded), frame);
                }
                "directory" => {
                    let decoded = decode_response(&frame).expect("decodes");
                    let DirectoryResponse::Directory(d) = &decoded else {
                        panic!("a directory");
                    };
                    let names: Vec<&str> = d.endpoints.iter().map(EndpointId::as_str).collect();
                    let expected: Vec<&str> = v["endpoints"]
                        .as_array()
                        .expect("list")
                        .iter()
                        .map(|e| e.as_str().expect("str"))
                        .collect();
                    assert_eq!(names, expected);
                    assert_eq!(encode_response(&decoded), frame);
                    assert!(frame.len() <= MAX_RESPONSE_BYTES);
                }
                other => panic!("unknown kind {other}"),
            }
            seen += 1;
        }
        assert_eq!(seen, 5, "every vector in the file was exercised");
    }

    #[test]
    fn the_ceiling_frame_is_exactly_the_response_bound() {
        let doc = fixture();
        let ceiling = doc["vectors"]
            .as_array()
            .expect("vectors")
            .iter()
            .find(|v| v["name"] == "ceiling-32-by-64")
            .expect("the ceiling vector");
        assert_eq!(
            ceiling["frame_len"].as_u64(),
            Some(MAX_RESPONSE_BYTES as u64)
        );
    }

    #[test]
    fn a_label_outside_the_grammar_does_not_decode() {
        let mut frame = encode_response(&DirectoryResponse::Directory(EndpointDirectoryV1 {
            generated_at_ms: 0,
            ttl_ms: 0,
            endpoints: vec![ep("human")],
        }));
        // Uppercase the first byte of the label: `Human` is outside
        // `^[a-z][a-z0-9._-]{0,63}$`.
        let label_at = 1 + 8 + 4 + 1 + 1;
        frame[label_at] = b'H';
        assert_eq!(decode_response(&frame), Err("entry outside the grammar"));
    }

    #[test]
    fn thirty_three_entries_do_not_decode() {
        let mut frame = vec![TAG_DIRECTORY];
        frame.extend_from_slice(&0u64.to_be_bytes());
        frame.extend_from_slice(&0u32.to_be_bytes());
        frame.push(33);
        for i in 0..33u8 {
            frame.push(3);
            frame.extend_from_slice(format!("e{i:02}").as_bytes());
        }
        assert_eq!(
            decode_response(&frame),
            Err("more entries than the frame may carry")
        );
    }

    #[test]
    fn trailing_bytes_and_bad_tags_are_refused() {
        let mut ok = encode_response(&DirectoryResponse::Refused(DirectoryRefusal::Overloaded));
        ok.push(0);
        assert!(decode_response(&ok).is_err());
        assert_eq!(decode_response(&[0x09]), Err("unknown response tag"));
        assert_eq!(
            decode_request(&[]),
            Err("an empty request is a closed stream, not a query")
        );
        assert!(decode_request(&[TAG_REQUEST, 0]).is_err());
    }

    #[test]
    fn an_unknown_refusal_code_reads_as_unavailable() {
        assert_eq!(
            decode_response(&[TAG_REFUSED, 200]),
            Ok(DirectoryResponse::Refused(DirectoryRefusal::Unavailable))
        );
    }
}
