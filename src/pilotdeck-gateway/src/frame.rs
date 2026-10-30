//! Retain the original wire, including nonminimal lengths, FIN/RSV and mask key.
use bytes::{Bytes, BytesMut};

pub(crate) const MAX_CLIENT_HEADER_BYTES: usize = 14;
pub(crate) const MAX_SERVER_HEADER_BYTES: usize = 10;

const MAX_SAFE_INTEGER: u64 = (1 << 53) - 1;

#[derive(Debug)]
pub(crate) struct Frame {
    pub opcode: u8,
    pub wire: Bytes,
    pub payload: Vec<u8>,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum DecodeError {
    Unmasked,
    TooLarge,
}

#[derive(Default)]
pub(crate) struct Decoder {
    pub buffer: BytesMut,
}

impl Decoder {
    pub fn next(&mut self) -> Result<Option<Frame>, DecodeError> {
        self.decode(true, MAX_SAFE_INTEGER)
    }

    /// Opt-in native listener bound, checked from the header before payload
    /// allocation. The transparent proxy retains the baseline's default.
    pub fn next_limited(&mut self, max_payload: u64) -> Result<Option<Frame>, DecodeError> {
        let max_payload = if crate::mutants::on("native-ws-input-boundary") {
            max_payload.saturating_sub(1)
        } else {
            max_payload
        };
        self.decode(true, max_payload)
    }

    pub fn next_server(&mut self) -> Result<Option<Frame>, DecodeError> {
        self.decode(false, MAX_SAFE_INTEGER)
    }

    fn decode(
        &mut self,
        require_mask: bool,
        max_payload: u64,
    ) -> Result<Option<Frame>, DecodeError> {
        let buf = &self.buffer;
        if buf.len() < 2 {
            return Ok(None);
        }
        let (size, offset) = match buf[1] & 0x7f {
            126 => {
                if buf.len() < 4 {
                    return Ok(None);
                }
                (u64::from(u16::from_be_bytes([buf[2], buf[3]])), 4)
            }
            127 => {
                if buf.len() < 10 {
                    return Ok(None);
                }
                let size = u64::from_be_bytes(buf[2..10].try_into().expect("eight bytes"));
                if size > MAX_SAFE_INTEGER {
                    return Err(DecodeError::TooLarge);
                }
                (size, 10)
            }
            size => (u64::from(size), 2),
        };
        if size > max_payload {
            return Err(DecodeError::TooLarge);
        }
        // TS checks masking after the extended length, before awaiting payload.
        let masked = buf[1] & 0x80 != 0;
        if require_mask && !masked {
            return Err(DecodeError::Unmasked);
        }
        let size = usize::try_from(size).map_err(|_| DecodeError::TooLarge)?;
        let payload_start = offset + if masked { 4 } else { 0 };
        let total = size
            .checked_add(payload_start)
            .ok_or(DecodeError::TooLarge)?;
        if buf.len() < total {
            return Ok(None);
        }
        let wire = self.buffer.split_to(total).freeze();
        let mut payload = wire[payload_start..].to_vec();
        if masked {
            for (index, byte) in payload.iter_mut().enumerate() {
                *byte ^= wire[offset + index % 4];
            }
        }
        Ok(Some(Frame {
            opcode: wire[0] & 15,
            wire,
            payload,
        }))
    }
}

/// TS-generated replies always use FIN=1, RSV=0, no mask, minimal length.
pub(crate) fn server_frame(opcode: u8, payload: &[u8]) -> Vec<u8> {
    let mut wire = vec![0x80 | opcode];
    match payload.len() {
        0..126 => wire.push(payload.len() as u8),
        126..=65535 => {
            wire.push(126);
            wire.extend_from_slice(&(payload.len() as u16).to_be_bytes());
        }
        _ => {
            wire.push(127);
            wire.extend_from_slice(&(payload.len() as u64).to_be_bytes());
        }
    }
    if crate::mutants::on("native-ws-server-header-budget") {
        wire.push(0);
    }
    wire.extend_from_slice(payload);
    wire
}

#[cfg(test)]
pub(crate) fn client_frame(first: u8, length_code: u8, payload: &[u8]) -> Vec<u8> {
    let mut wire = vec![first, 0x80 | length_code];
    match length_code {
        126 => wire.extend_from_slice(&(payload.len() as u16).to_be_bytes()),
        127 => wire.extend_from_slice(&(payload.len() as u64).to_be_bytes()),
        _ => assert_eq!(usize::from(length_code), payload.len()),
    }
    if crate::mutants::on("native-ws-client-header-budget") {
        wire.push(0);
    }
    let mask = [1, 2, 3, 4];
    wire.extend_from_slice(&mask);
    wire.extend(payload.iter().enumerate().map(|(i, b)| b ^ mask[i % 4]));
    wire
}
