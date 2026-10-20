//! `readServerSentEvents` / `parseServerSentEventChunk` of `streamModel.ts`.
//!
//! Byte chunks go through a WHATWG `TextDecoder("utf-8")` in stream mode (BOM
//! stripped once, maximal-subpart U+FFFD replacement, a split sequence is
//! carried to the next chunk). The decoded buffer is split on `"\n\n"` only,
//! like the baseline: a CRLF-delimited event waits for a later LF-LF.
use pilotdeck_protocol::{JsValue, parse_json};

#[derive(Default)]
pub(crate) struct Utf8Decoder {
    pending: Vec<u8>,
    bom_checked: bool,
}
impl Utf8Decoder {
    pub(crate) fn decode(&mut self, chunk: &[u8], out: &mut String) {
        self.pending.extend_from_slice(chunk);
        if !self.bom_checked {
            if self.pending.len() < 3 && b"\xEF\xBB\xBF".starts_with(&self.pending) {
                return;
            }
            self.bom_checked = true;
            if self.pending.starts_with(b"\xEF\xBB\xBF") {
                self.pending.drain(..3);
            }
        }
        let mut start = 0;
        loop {
            match std::str::from_utf8(&self.pending[start..]) {
                Ok(valid) => {
                    out.push_str(valid);
                    self.pending.clear();
                    return;
                }
                Err(error) => {
                    let valid = start + error.valid_up_to();
                    out.push_str(
                        std::str::from_utf8(&self.pending[start..valid]).expect("validated prefix"),
                    );
                    match error.error_len() {
                        Some(len) => {
                            out.push('\u{FFFD}');
                            start = valid + len;
                        }
                        None => {
                            // Incomplete trailing sequence: keep for the next chunk.
                            self.pending.drain(..valid);
                            return;
                        }
                    }
                }
            }
        }
    }
    /// End of stream: an incomplete sequence becomes one U+FFFD.
    pub(crate) fn finish(&mut self, out: &mut String) {
        if !self.bom_checked {
            self.bom_checked = true;
            if self.pending == b"\xEF\xBB\xBF" {
                self.pending.clear();
            }
        }
        if !self.pending.is_empty() {
            out.push_str(&String::from_utf8_lossy(&self.pending));
            self.pending.clear();
        }
    }
}

#[derive(Debug, PartialEq)]
pub(crate) enum SseEvent {
    Data(JsValue),
    Done,
}

/// A `data:` line whose payload is not JSON: `JSON.parse` throws SyntaxError.
#[derive(Debug)]
pub(crate) struct SyntaxError(pub String);

/// Splits the decoded buffer on `"\n\n"`; complete chunks become events.
#[derive(Default)]
pub(crate) struct SseBuffer {
    decoder: Utf8Decoder,
    buffer: String,
}
impl SseBuffer {
    /// Feed one network chunk; returns the parsed chunks in order. A JSON
    /// error stops at that line, like the throwing generator.
    pub(crate) fn push(&mut self, bytes: &[u8]) -> Vec<Result<SseEvent, SyntaxError>> {
        self.decoder.decode(bytes, &mut self.buffer);
        let mut events = Vec::new();
        while let Some(at) = self.buffer.find("\n\n") {
            let chunk: String = self.buffer[..at].to_string();
            self.buffer.drain(..at + 2);
            if parse_chunk(&chunk, &mut events) {
                break;
            }
        }
        events
    }
    /// Stream end: flush the decoder and parse a non-blank remainder.
    pub(crate) fn finish(&mut self) -> Vec<Result<SseEvent, SyntaxError>> {
        self.decoder.finish(&mut self.buffer);
        let mut events = Vec::new();
        let rest = std::mem::take(&mut self.buffer);
        if !crate::metadata::js_trim(&rest).is_empty() {
            parse_chunk(&rest, &mut events);
        }
        events
    }
}

/// Returns true when a SyntaxError was produced (nothing after it is parsed).
fn parse_chunk(chunk: &str, events: &mut Vec<Result<SseEvent, SyntaxError>>) -> bool {
    for line in chunk.split('\n') {
        let Some(data) = line.strip_prefix("data:") else {
            continue;
        };
        let data = crate::metadata::js_trim(data);
        if data.is_empty() {
            continue;
        }
        if data == "[DONE]" {
            events.push(Ok(SseEvent::Done));
            continue;
        }
        match parse_json(data) {
            Ok(value) => events.push(Ok(SseEvent::Data(value))),
            Err(_) => {
                let message = pilotdeck_protocol::json_syntax::parse_error(data)
                    .unwrap_or_else(|| "Unexpected end of JSON input".into());
                events.push(Err(SyntaxError(message)));
                return true;
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode_all(chunks: &[&[u8]]) -> String {
        let mut decoder = Utf8Decoder::default();
        let mut out = String::new();
        for chunk in chunks {
            decoder.decode(chunk, &mut out);
        }
        decoder.finish(&mut out);
        out
    }

    #[test]
    fn decoder_carries_split_sequences_and_strips_one_bom() {
        let text = "\u{feff}\u{feff}中😀é".as_bytes();
        let pieces: Vec<&[u8]> = text.chunks(1).collect();
        assert_eq!(decode_all(&pieces), "\u{feff}中😀é");
        assert_eq!(decode_all(&[b"\xEF", b"\xBB"]), "\u{FFFD}");
        assert_eq!(decode_all(&[b"a\xFFb\xE4\xB8"]), "a\u{FFFD}b\u{FFFD}");
    }

    #[test]
    fn crlf_event_waits_for_lf_lf_and_trims_carriage_return() {
        let mut sse = SseBuffer::default();
        assert!(sse.push(b"data: {\"a\":1}\r\n\r\n").is_empty());
        let events = sse.push(b"data: 2\n\n");
        let values: Vec<_> = events.into_iter().map(|e| e.unwrap()).collect();
        assert_eq!(values.len(), 2);
    }
}
