//! Node 22 / V8 12.4 diagnostics for provider Response.json(). Valid values
//! still use the shared codec; this iterative scanner runs only on failure.
use crate::host::{HostError, ModuleError};
use pilotdeck_protocol::{JsString, JsValue, parse_json};

pub(super) fn parse_response(bytes: &[u8]) -> Result<JsValue, HostError> {
    // Node 22's undici strips a byte BOM, then TextDecoder strips one more.
    let bytes = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(bytes);
    let text = String::from_utf8_lossy(bytes);
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
    parse_json(text).map_err(|_| ModuleError::new(Diagnostic::new(text).message()).into())
}

struct Diagnostic {
    units: Vec<u16>,
    at: usize,
}
enum State {
    Value,
    End,
    ArrayFirst,
    ArrayEnd,
    ObjectKey(bool),
    ObjectEnd,
}
impl Diagnostic {
    fn new(text: &str) -> Self {
        Self {
            units: text.encode_utf16().collect(),
            at: 0,
        }
    }
    fn peek(&self) -> Option<u16> {
        self.units.get(self.at).copied()
    }
    fn eat(&mut self, c: u8) -> bool {
        if self.peek() == Some(u16::from(c)) {
            self.at += 1;
            true
        } else {
            false
        }
    }
    fn whitespace(&mut self) {
        while matches!(self.peek(), Some(9 | 10 | 13 | 32)) {
            self.at += 1;
        }
    }
    fn position(&self, message: &str) -> JsString {
        let mut line = 1;
        let mut start = 0;
        for i in 0..self.at {
            if self.units[i] == 13 || (self.units[i] == 10 && (i == 0 || self.units[i - 1] != 13)) {
                line += 1;
            }
            if matches!(self.units[i], 10 | 13) {
                start = i + 1;
            }
        }
        format!(
            "{message} at position {} (line {line} column {})",
            self.at,
            self.at - start + 1
        )
        .into()
    }
    fn unexpected(&self) -> JsString {
        let Some(token) = self.peek() else {
            return "Unexpected end of JSON input".into();
        };
        if matches!(token, 45 | 48..=57) {
            return self.position("Unexpected number in JSON");
        }
        if token == 34 {
            return self.position("Unexpected string in JSON");
        }
        let text = String::from_utf16_lossy(&self.units);
        if ["undefined", "NaN", "Infinity", "[object Object]"].contains(&text.as_str()) {
            return format!("\"{text}\" is not valid JSON").into();
        }
        let mut out: Vec<u16> = "Unexpected token '".encode_utf16().collect();
        out.push(token);
        out.extend("', ".encode_utf16());
        let long = self.units.len() > 20;
        if long && self.at >= 10 {
            out.extend("...".encode_utf16());
        }
        out.push(34);
        let start = if long { self.at.saturating_sub(10) } else { 0 };
        let end = if long {
            (self.at + 10).min(self.units.len())
        } else {
            self.units.len()
        };
        out.extend_from_slice(&self.units[start..end]);
        out.push(34);
        if long && self.at + 10 < self.units.len() {
            out.extend("...".encode_utf16());
        }
        out.extend(" is not valid JSON".encode_utf16());
        JsString::from_utf16(out)
    }
    fn string(&mut self) -> Result<(), JsString> {
        self.at += 1; // opening quote
        loop {
            match self.peek() {
                None => return Err(self.position("Unterminated string in JSON")),
                Some(34) => {
                    self.at += 1;
                    return Ok(());
                }
                Some(92) => {
                    self.at += 1;
                    match self.peek() {
                        None => return Err(self.unexpected()),
                        Some(117) => {
                            for _ in 0..4 {
                                self.at += 1;
                                if !matches!(self.peek(), Some(48..=57 | 65..=70 | 97..=102)) {
                                    return Err(self.position("Bad Unicode escape in JSON"));
                                }
                            }
                            self.at += 1;
                        }
                        Some(34 | 47 | 92 | 98 | 102 | 110 | 114 | 116) => self.at += 1,
                        // V8 >=256 prints "Unexpected token '<unit>'" for a
                        // backslash before a non-ASCII unit, not "Bad escaped
                        // character" (measured Node 22.22, JSON.parse(`"a\\Ā"`)).
                        // json-invalid-81 in the HTTP oracle pins this.
                        Some(256..) => return Err(self.unexpected()),
                        _ => return Err(self.position("Bad escaped character in JSON")),
                    }
                }
                Some(0..=31) => {
                    return Err(self.position("Bad control character in string literal in JSON"));
                }
                _ => self.at += 1,
            }
        }
    }
    fn number(&mut self) -> Result<(), JsString> {
        if self.eat(b'-') && !matches!(self.peek(), Some(48..=57)) {
            return Err(self.position("No number after minus sign in JSON"));
        }
        if self.eat(b'0') {
            if matches!(self.peek(), Some(48..=57)) {
                return Err(self.position("Unexpected number in JSON"));
            }
        } else {
            while matches!(self.peek(), Some(48..=57)) {
                self.at += 1;
            }
        }
        if self.eat(b'.') {
            if !matches!(self.peek(), Some(48..=57)) {
                return Err(self.position("Unterminated fractional number in JSON"));
            }
            while matches!(self.peek(), Some(48..=57)) {
                self.at += 1;
            }
        }
        if self.eat(b'e') || self.eat(b'E') {
            if !self.eat(b'+') {
                self.eat(b'-');
            }
            if !matches!(self.peek(), Some(48..=57)) {
                return Err(self.position("Exponent part is missing a number in JSON"));
            }
            while matches!(self.peek(), Some(48..=57)) {
                self.at += 1;
            }
        }
        Ok(())
    }
    fn scan(&mut self) -> Result<(), JsString> {
        let mut states = vec![State::End, State::Value];
        while let Some(state) = states.pop() {
            self.whitespace();
            match state {
                State::Value => match self.peek() {
                    Some(123) => {
                        self.at += 1;
                        states.push(State::ObjectKey(true));
                    }
                    Some(91) => {
                        self.at += 1;
                        states.push(State::ArrayFirst);
                    }
                    Some(34) => self.string()?,
                    Some(45 | 48..=57) => self.number()?,
                    Some(c @ (110 | 116 | 102)) => {
                        let literal: &[u8] = match c {
                            110 => b"null",
                            116 => b"true",
                            _ => b"false",
                        };
                        for &expected in literal {
                            if !self.eat(expected) {
                                return Err(self.unexpected());
                            }
                        }
                    }
                    _ => return Err(self.unexpected()),
                },
                State::End => {
                    if self.peek().is_some() {
                        return Err(self.position("Unexpected non-whitespace character after JSON"));
                    }
                }
                State::ArrayFirst => {
                    if !self.eat(b']') {
                        states.extend([State::ArrayEnd, State::Value]);
                    }
                }
                State::ArrayEnd => {
                    if !self.eat(b']') {
                        if !self.eat(b',') {
                            return Err(
                                self.position("Expected ',' or ']' after array element in JSON")
                            );
                        }
                        states.extend([State::ArrayEnd, State::Value]);
                    }
                }
                State::ObjectKey(first) => {
                    if first && self.eat(b'}') {
                        continue;
                    }
                    if self.peek() != Some(34) {
                        return Err(self.position(if first {
                            "Expected property name or '}' in JSON"
                        } else {
                            "Expected double-quoted property name in JSON"
                        }));
                    }
                    self.string()?;
                    self.whitespace();
                    if !self.eat(b':') {
                        return Err(self.position("Expected ':' after property name in JSON"));
                    }
                    states.extend([State::ObjectEnd, State::Value]);
                }
                State::ObjectEnd => {
                    if !self.eat(b'}') {
                        if !self.eat(b',') {
                            return Err(
                                self.position("Expected ',' or '}' after property value in JSON")
                            );
                        }
                        states.push(State::ObjectKey(false));
                    }
                }
            }
        }
        Ok(())
    }
    fn message(mut self) -> JsString {
        self.scan().expect_err("shared codec rejected JSON syntax")
    }
}
