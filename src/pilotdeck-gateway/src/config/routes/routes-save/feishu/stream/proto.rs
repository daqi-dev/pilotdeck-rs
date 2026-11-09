//! pbbp2 Frame from the fixed lark SDK (lib/index.js:88491-88599).
use super::*;

#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct Frame {
    pub seq: u64,
    pub log: u64,
    pub service: u64,
    pub method: u64,
    pub headers: Vec<(String, String)>,
    pub encoding: Option<Vec<u8>>,
    pub payload_type: Option<Vec<u8>>,
    pub payload: Option<Vec<u8>>,
    pub log_new: Option<Vec<u8>>,
}
impl Frame {
    pub fn header(&self, key: &str) -> Option<&str> {
        self.headers
            .iter()
            .rev()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value.as_str())
    }
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        for (key, value) in [
            (1, self.seq),
            (2, self.log),
            (3, self.service),
            (4, self.method),
        ] {
            varint(&mut out, key << 3);
            varint(&mut out, value);
        }
        for (key, value) in &self.headers {
            let mut header = Vec::new();
            bytes(&mut header, 1, key.as_bytes());
            bytes(&mut header, 2, value.as_bytes());
            bytes(&mut out, 5, &header);
        }
        for (key, value) in [
            (6, &self.encoding),
            (7, &self.payload_type),
            (8, &self.payload),
            (9, &self.log_new),
        ] {
            if let Some(value) = value {
                bytes(&mut out, key, value);
            }
        }
        out
    }
    pub fn decode(raw: &[u8]) -> Result<Self> {
        let mut reader = Reader { raw, at: 0 };
        let mut frame = Self::default();
        let mut required = 0;
        while reader.at < raw.len() {
            let tag = reader.varint()?;
            let key = tag >> 3;
            match (key, tag & 7) {
                (1..=4, 0) => {
                    let value = reader.varint()?;
                    match key {
                        1 => frame.seq = value,
                        2 => frame.log = value,
                        3 => frame.service = value,
                        _ => frame.method = value,
                    }
                    required |= 1 << (key - 1);
                }
                (5, 2) => {
                    let mut reader = Reader {
                        raw: reader.bytes()?,
                        at: 0,
                    };
                    let (mut key, mut value) = (None, None);
                    while reader.at < reader.raw.len() {
                        let tag = reader.varint()?;
                        match tag {
                            10 => key = Some(reader.string()?),
                            18 => value = Some(reader.string()?),
                            _ => reader.skip(tag & 7)?,
                        }
                    }
                    frame.headers.push((
                        key.ok_or_else(|| failure("Missing Feishu header key"))?,
                        value.ok_or_else(|| failure("Missing Feishu header value"))?,
                    ));
                }
                (6..=9, 2) => {
                    let value = Some(reader.bytes()?.to_vec());
                    match key {
                        6 => frame.encoding = value,
                        7 => frame.payload_type = value,
                        8 => frame.payload = value,
                        _ => frame.log_new = value,
                    }
                }
                (_, wire) => reader.skip(wire)?,
            }
        }
        if required != 15 {
            return Err(failure("Missing required Feishu protobuf field"));
        }
        Ok(frame)
    }
}
fn varint(out: &mut Vec<u8>, mut value: u64) {
    while value >= 128 {
        out.push(value as u8 | 0x80);
        value >>= 7;
    }
    out.push(value as u8);
}
fn bytes(out: &mut Vec<u8>, key: u64, value: &[u8]) {
    varint(out, key << 3 | 2);
    varint(out, value.len() as u64);
    out.extend_from_slice(value);
}
struct Reader<'a> {
    raw: &'a [u8],
    at: usize,
}
impl<'a> Reader<'a> {
    fn varint(&mut self) -> Result<u64> {
        let mut value = 0;
        for shift in (0..70).step_by(7) {
            let byte = *self
                .raw
                .get(self.at)
                .ok_or_else(|| failure("Truncated Feishu protobuf"))?;
            self.at += 1;
            if shift == 63 && byte > 1 {
                return Err(failure("Invalid Feishu protobuf varint"));
            }
            value |= u64::from(byte & 127) << shift;
            if byte & 128 == 0 {
                return Ok(value);
            }
        }
        Err(failure("Invalid Feishu protobuf varint"))
    }
    fn bytes(&mut self) -> Result<&'a [u8]> {
        let length = usize::try_from(self.varint()?)
            .map_err(|_| failure("Invalid Feishu protobuf length"))?;
        let end = self
            .at
            .checked_add(length)
            .ok_or_else(|| failure("Invalid Feishu protobuf length"))?;
        let value = self
            .raw
            .get(self.at..end)
            .ok_or_else(|| failure("Truncated Feishu protobuf bytes"))?;
        self.at = end;
        Ok(value)
    }
    fn string(&mut self) -> Result<String> {
        Ok(String::from_utf8_lossy(self.bytes()?).into_owned())
    }
    fn skip(&mut self, wire: u64) -> Result<()> {
        match wire {
            0 => {
                self.varint()?;
            }
            1 => {
                self.take(8)?;
            }
            2 => {
                self.bytes()?;
            }
            5 => {
                self.take(4)?;
            }
            _ => return Err(failure("Invalid Feishu protobuf wire type")),
        };
        Ok(())
    }
    fn take(&mut self, n: usize) -> Result<()> {
        let end = self
            .at
            .checked_add(n)
            .filter(|end| *end <= self.raw.len())
            .ok_or_else(|| failure("Truncated Feishu protobuf"))?;
        self.at = end;
        Ok(())
    }
}
