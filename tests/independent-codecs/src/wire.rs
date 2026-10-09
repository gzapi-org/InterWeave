// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton

//! A cursor over a frame. Every length is checked against what remains
//! BEFORE anything is taken, so a declared length larger than the frame is
//! refused without allocating it (`DIRECT.md` §Request).

use crate::DecodeError;

pub(crate) struct Reader<'a> {
    rest: &'a [u8],
}

impl<'a> Reader<'a> {
    pub(crate) fn new(bytes: &'a [u8]) -> Self {
        Self { rest: bytes }
    }

    pub(crate) fn take(&mut self, n: usize, what: &str) -> Result<&'a [u8], DecodeError> {
        if self.rest.len() < n {
            return Err(DecodeError(format!(
                "{what}: {n} byte(s) declared, {} left",
                self.rest.len()
            )));
        }
        let (head, tail) = self.rest.split_at(n);
        self.rest = tail;
        Ok(head)
    }

    pub(crate) fn u8(&mut self, what: &str) -> Result<u8, DecodeError> {
        Ok(self.take(1, what)?[0])
    }

    pub(crate) fn u32be(&mut self, what: &str) -> Result<u32, DecodeError> {
        let b = self.take(4, what)?;
        Ok(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }

    pub(crate) fn u64be(&mut self, what: &str) -> Result<u64, DecodeError> {
        let b = self.take(8, what)?;
        let mut a = [0u8; 8];
        a.copy_from_slice(b);
        Ok(u64::from_be_bytes(a))
    }

    pub(crate) fn id16(&mut self) -> Result<[u8; 16], DecodeError> {
        let b = self.take(16, "message_id")?;
        let mut a = [0u8; 16];
        a.copy_from_slice(b);
        Ok(a)
    }

    /// The frame is the whole substream: a byte past the last field is
    /// not a field nobody asked about, it is a different frame.
    pub(crate) fn finish(self) -> Result<(), DecodeError> {
        if self.rest.is_empty() {
            Ok(())
        } else {
            Err(DecodeError(format!(
                "{} byte(s) after the payload",
                self.rest.len()
            )))
        }
    }
}

/// `media_type_len:u8 || media_type`, zero meaning absent.
pub(crate) fn media_type(r: &mut Reader<'_>) -> Result<Option<String>, DecodeError> {
    let len = usize::from(r.u8("media_type_len")?);
    if len == 0 {
        return Ok(None);
    }
    let bytes = r.take(len, "media_type")?;
    let text = core::str::from_utf8(bytes)
        .map_err(|_| DecodeError("media_type is not ASCII".to_owned()))?;
    if !crate::is_media_type(text) {
        return Err(DecodeError(format!(
            "media_type is not 1..{} ASCII bytes",
            crate::MAX_MEDIA_TYPE_BYTES
        )));
    }
    Ok(Some(text.to_owned()))
}

/// `payload_len:u32be || payload`, bounded before the bytes are taken.
pub(crate) fn payload(r: &mut Reader<'_>) -> Result<Vec<u8>, DecodeError> {
    let len = r.u32be("payload_len")?;
    let len = usize::try_from(len).unwrap_or(usize::MAX);
    if len > crate::MAX_PAYLOAD_BYTES {
        return Err(DecodeError(format!(
            "payload_len {len} above {}",
            crate::MAX_PAYLOAD_BYTES
        )));
    }
    Ok(r.take(len, "payload")?.to_vec())
}

pub(crate) fn put_media_type(out: &mut Vec<u8>, media_type: Option<&str>) {
    match media_type {
        None => out.push(0),
        Some(m) => {
            out.push(u8::try_from(m.len()).unwrap_or(u8::MAX));
            out.extend_from_slice(m.as_bytes());
        }
    }
}

pub(crate) fn put_payload(out: &mut Vec<u8>, payload: &[u8]) {
    out.extend_from_slice(
        &u32::try_from(payload.len())
            .unwrap_or(u32::MAX)
            .to_be_bytes(),
    );
    out.extend_from_slice(payload);
}
