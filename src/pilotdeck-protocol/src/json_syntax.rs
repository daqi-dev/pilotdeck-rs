//! SyntaxError messages of V8 12.x (Node 22) JSON.parse, over UTF-16 code units.
//! Only validates; the parsed value is not needed before the SDK call.

use crate::JsString;

const MAX_CONTEXT: usize = 10;
const MIN_LENGTH_FOR_CONTEXT: usize = MAX_CONTEXT * 2 + 1;

const PROP_NAME_OR_RBRACE: &str = "Expected property name or '}' in JSON";
const DOUBLE_QUOTED_PROP_NAME: &str = "Expected double-quoted property name in JSON";
const COLON_AFTER_PROP_NAME: &str = "Expected ':' after property name in JSON";
const COMMA_OR_RBRACE: &str = "Expected ',' or '}' after property value in JSON";
const COMMA_OR_RBRACK: &str = "Expected ',' or ']' after array element in JSON";
const NON_WHITESPACE_AFTER: &str = "Unexpected non-whitespace character after JSON";
const UNTERMINATED_STRING: &str = "Unterminated string in JSON";
const BAD_CONTROL_CHARACTER: &str = "Bad control character in string literal in JSON";
const BAD_UNICODE_ESCAPE: &str = "Bad Unicode escape in JSON";
const BAD_ESCAPED_CHARACTER: &str = "Bad escaped character in JSON";
const NO_NUMBER_AFTER_MINUS: &str = "No number after minus sign in JSON";
const UNTERMINATED_FRACTION: &str = "Unterminated fractional number in JSON";
const EXPONENT_MISSING_NUMBER: &str = "Exponent part is missing a number in JSON";

#[derive(Clone, Copy)]
enum Token {
    Eos,
    Number,
    String,
    Other,
}

fn token_of(unit: Option<u16>) -> Token {
    match unit {
        None => Token::Eos,
        Some(0x22) => Token::String,
        Some(0x2d | 0x30..=0x39) => Token::Number,
        Some(_) => Token::Other,
    }
}

fn is_digit(unit: Option<u16>) -> bool {
    matches!(unit, Some(0x30..=0x39))
}

/// None when the text is valid JSON; otherwise the exact SyntaxError message.
/// This string API is lossy for a message that quotes a lone surrogate; callers
/// that need the original units use [`parse_error_units`].
pub fn parse_error(source: &str) -> Option<String> {
    let units: Vec<u16> = source.encode_utf16().collect();
    parse_error_units(&units).map(|message| String::from_utf16_lossy(message.units()))
}

/// UTF-16 entry point: keeps lone surrogates that V8 quotes in the diagnostic.
pub fn parse_error_units(source: &[u16]) -> Option<JsString> {
    Scanner { s: source, i: 0 }.parse().err()
}

struct Scanner<'a> {
    s: &'a [u16],
    i: usize,
}

type Scan = Result<(), JsString>;

fn push_str(out: &mut Vec<u16>, text: &str) {
    out.extend(text.encode_utf16());
}

fn quoted(units: &[u16]) -> JsString {
    let mut out = Vec::with_capacity(units.len() + 2);
    out.push(u16::from(b'"'));
    out.extend_from_slice(units);
    out.push(u16::from(b'"'));
    JsString::from_utf16(out)
}

impl Scanner<'_> {
    fn peek(&self) -> Option<u16> {
        self.s.get(self.i).copied()
    }

    fn skip_whitespace(&mut self) {
        while matches!(self.peek(), Some(0x20 | 0x09 | 0x0a | 0x0d)) {
            self.i += 1;
        }
    }

    fn line_column(&self) -> (usize, usize) {
        let (mut line, mut last_break, mut k) = (1, 0, 0);
        while k < self.i {
            if self.s[k] == 0x0d && k + 1 < self.i && self.s[k + 1] == 0x0a {
                k += 1;
            }
            if matches!(self.s[k], 0x0d | 0x0a) {
                line += 1;
                last_break = k + 1;
            }
            k += 1;
        }
        (line, 1 + self.i - last_break)
    }

    fn at_position(&self, prefix: &str) -> JsString {
        let (line, column) = self.line_column();
        let mut out = Vec::new();
        push_str(&mut out, prefix);
        push_str(
            &mut out,
            &format!(" at position {} (line {line} column {column})", self.i),
        );
        JsString::from_utf16(out)
    }

    fn report(&self, token: Token, message: Option<&str>) -> JsString {
        if let Some(prefix) = message {
            return self.at_position(prefix);
        }
        match token {
            Token::Eos => "Unexpected end of JSON input".into(),
            Token::Number => self.at_position("Unexpected number in JSON"),
            Token::String => self.at_position("Unexpected string in JSON"),
            Token::Other => self.unexpected_token(),
        }
    }

    fn unexpected_token(&self) -> JsString {
        let source = self.s;
        if ["[object Object]", "undefined", "Infinity", "NaN"]
            .iter()
            .any(|special| special.encode_utf16().eq(source.iter().copied()))
        {
            let mut out = Vec::new();
            out.extend(quoted(source).units());
            push_str(&mut out, " is not valid JSON");
            return JsString::from_utf16(out);
        }
        let (pos, len) = (self.i, source.len());
        let token = &source[pos..pos + 1];
        let (before, ellipsis_before, ellipsis_after) = if len < MIN_LENGTH_FOR_CONTEXT {
            (source, false, false)
        } else if pos < MAX_CONTEXT {
            (&source[..pos + MAX_CONTEXT], false, true)
        } else if pos < len - MAX_CONTEXT {
            (&source[pos - MAX_CONTEXT..pos + MAX_CONTEXT], true, true)
        } else {
            (&source[pos - MAX_CONTEXT..], true, false)
        };
        let mut out = Vec::new();
        push_str(&mut out, "Unexpected token '");
        out.extend_from_slice(token);
        push_str(&mut out, "', ");
        if ellipsis_before {
            push_str(&mut out, "...");
        }
        out.extend(quoted(before).units());
        if ellipsis_after {
            push_str(&mut out, "...");
        }
        push_str(&mut out, " is not valid JSON");
        JsString::from_utf16(out)
    }

    fn fail(&self, message: &str) -> Scan {
        Err(self.report(Token::Other, Some(message)))
    }

    fn unexpected(&self) -> Scan {
        Err(self.report(token_of(self.peek()), None))
    }

    /// ExpectNext: the reported message wins over the actual token, EOS included.
    fn expect(&mut self, unit: u16, message: &str) -> Scan {
        self.skip_whitespace();
        if self.peek() != Some(unit) {
            return self.fail(message);
        }
        if unit == 0x22 {
            self.scan_string()
        } else {
            self.i += 1;
            Ok(())
        }
    }

    fn parse(mut self) -> Scan {
        let mut stack: Vec<bool> = Vec::new();
        'value: loop {
            self.skip_whitespace();
            match self.peek() {
                Some(0x22) => self.scan_string()?,
                Some(0x2d | 0x30..=0x39) => self.scan_number()?,
                Some(0x7b) => {
                    self.i += 1;
                    self.skip_whitespace();
                    if self.peek() == Some(0x7d) {
                        self.i += 1;
                    } else {
                        stack.push(true);
                        self.expect(0x22, PROP_NAME_OR_RBRACE)?;
                        self.expect(0x3a, COLON_AFTER_PROP_NAME)?;
                        continue 'value;
                    }
                }
                Some(0x5b) => {
                    self.i += 1;
                    self.skip_whitespace();
                    if self.peek() == Some(0x5d) {
                        self.i += 1;
                    } else {
                        stack.push(false);
                        continue 'value;
                    }
                }
                Some(0x74) => self.scan_literal("true")?,
                Some(0x66) => self.scan_literal("false")?,
                Some(0x6e) => self.scan_literal("null")?,
                _ => return self.unexpected(),
            }
            while let Some(&is_object) = stack.last() {
                self.skip_whitespace();
                if self.peek() == Some(0x2c) {
                    self.i += 1;
                    if is_object {
                        self.expect(0x22, DOUBLE_QUOTED_PROP_NAME)?;
                        self.expect(0x3a, COLON_AFTER_PROP_NAME)?;
                    }
                    continue 'value;
                }
                if is_object {
                    self.expect(0x7d, COMMA_OR_RBRACE)?;
                } else {
                    self.expect(0x5d, COMMA_OR_RBRACK)?;
                }
                stack.pop();
            }
            break;
        }
        self.skip_whitespace();
        if self.peek().is_some() {
            return self.fail(NON_WHITESPACE_AFTER);
        }
        Ok(())
    }

    fn scan_string(&mut self) -> Scan {
        self.i += 1;
        loop {
            let Some(unit) = self.peek() else {
                return self.fail(UNTERMINATED_STRING);
            };
            match unit {
                0x22 => {
                    self.i += 1;
                    return Ok(());
                }
                0x5c => {
                    self.i += 1;
                    match self.peek() {
                        // V8 reports EOS right after a backslash as the end token.
                        None => return Err(self.report(Token::Eos, None)),
                        Some(0x22 | 0x5c | 0x2f | 0x62 | 0x66 | 0x6e | 0x72 | 0x74) => self.i += 1,
                        Some(0x75) => {
                            for _ in 0..4 {
                                self.i += 1;
                                if !self.peek().is_some_and(|u| {
                                    char::from_u32(u32::from(u))
                                        .is_some_and(|c| c.is_ascii_hexdigit())
                                }) {
                                    return self.fail(BAD_UNICODE_ESCAPE);
                                }
                            }
                            self.i += 1;
                        }
                        // V8 >=256 prints "Unexpected token '<unit>'" for a
                        // backslash before a non-ASCII unit instead of "Bad
                        // escaped character" (measured Node 22; http oracle
                        // json-invalid-81 pins this).
                        Some(0x100..) => return self.unexpected(),
                        Some(_) => return self.fail(BAD_ESCAPED_CHARACTER),
                    }
                }
                0x00..=0x1f => return self.fail(BAD_CONTROL_CHARACTER),
                _ => self.i += 1,
            }
        }
    }

    fn skip_digits(&mut self) {
        while is_digit(self.peek()) {
            self.i += 1;
        }
    }

    fn scan_number(&mut self) -> Scan {
        if self.peek() == Some(0x2d) {
            self.i += 1;
        }
        if self.peek() == Some(0x30) {
            self.i += 1;
            if is_digit(self.peek()) {
                return Err(self.report(Token::Number, None));
            }
        } else if !is_digit(self.peek()) {
            return self.fail(NO_NUMBER_AFTER_MINUS);
        } else {
            self.skip_digits();
        }
        if self.peek() == Some(0x2e) {
            self.i += 1;
            if !is_digit(self.peek()) {
                return self.fail(UNTERMINATED_FRACTION);
            }
            self.skip_digits();
        }
        if matches!(self.peek(), Some(0x65 | 0x45)) {
            self.i += 1;
            if matches!(self.peek(), Some(0x2b | 0x2d)) {
                self.i += 1;
            }
            if !is_digit(self.peek()) {
                return self.fail(EXPONENT_MISSING_NUMBER);
            }
            self.skip_digits();
        }
        Ok(())
    }

    fn scan_literal(&mut self, word: &str) -> Scan {
        let word: Vec<u16> = word.encode_utf16().collect();
        let remaining = self.s.len() - self.i;
        if remaining >= word.len() && self.s[self.i..self.i + word.len()] == word[..] {
            self.i += word.len();
            return Ok(());
        }
        self.i += 1;
        for expected in &word[1..word.len().min(remaining)] {
            if self.peek() != Some(*expected) {
                return self.unexpected();
            }
            self.i += 1;
        }
        Err(self.report(Token::Eos, None))
    }
}

#[cfg(test)]
mod tests {
    use super::parse_error;

    #[test]
    fn valid_json_has_no_error() {
        for text in [
            "{}",
            " [1, -0.5e+3, \"a\\u00e9\", true, null, {\"k\": []}] ",
            "0",
            "\"\"",
        ] {
            assert_eq!(parse_error(text), None, "{text}");
        }
    }

    /// Host Node 22 JSON.parse messages recorded by tests/fixtures/json-syntax/generate.mjs.
    #[test]
    fn matches_recorded_node22_messages() {
        let fixture: serde_json::Value =
            serde_json::from_str(include_str!("../tests/fixtures/json-syntax/cases.json")).unwrap();
        assert!(fixture["node"].as_str().unwrap().starts_with("v22."));
        let cases = fixture["cases"].as_array().unwrap();
        assert!(cases.len() >= 100, "fixture has only {} cases", cases.len());
        let mut failures = Vec::new();
        for case in cases {
            let input = case["input"].as_str().unwrap();
            let expected = case["message"].as_str();
            let actual = parse_error(input);
            if actual.as_deref() != expected {
                failures.push(format!("{input:?}: expected {expected:?}, got {actual:?}"));
            }
        }
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    #[test]
    fn host5_recorded_open_brace_message() {
        assert_eq!(
            parse_error("{").as_deref(),
            Some("Expected property name or '}' in JSON at position 1 (line 1 column 2)")
        );
    }

    #[test]
    fn utf16_entry_point_keeps_lone_surrogates_that_string_api_loses() {
        // `"\<lone high surrogate>"` (not a `\u` escape): V8 quotes the raw
        // code unit in the token and context.
        let source = [0x22, 0x5c, 0xd800, 0x22];
        let message = super::parse_error_units(&source).unwrap();
        assert_eq!(
            message.units(),
            &[
                0x55, 0x6e, 0x65, 0x78, 0x70, 0x65, 0x63, 0x74, 0x65, 0x64, 0x20, 0x74, 0x6f, 0x6b,
                0x65, 0x6e, 0x20, 0x27, 0xd800, 0x27, 0x2c, 0x20, 0x22, 0x22, 0x5c, 0xd800, 0x22,
                0x22, 0x20, 0x69, 0x73, 0x20, 0x6e, 0x6f, 0x74, 0x20, 0x76, 0x61, 0x6c, 0x69, 0x64,
                0x20, 0x4a, 0x53, 0x4f, 0x4e
            ]
        );
        assert!(
            super::parse_error(&String::from_utf16_lossy(&source))
                .unwrap()
                .contains('\u{fffd}')
        );
    }

    #[test]
    fn eos_and_token_messages() {
        assert_eq!(
            parse_error("[").as_deref(),
            Some("Unexpected end of JSON input")
        );
        assert_eq!(
            parse_error("tru").as_deref(),
            Some("Unexpected end of JSON input")
        );
        assert_eq!(
            parse_error("01").as_deref(),
            Some("Unexpected number in JSON at position 1 (line 1 column 2)")
        );
        assert_eq!(
            parse_error("NaN").as_deref(),
            Some("\"NaN\" is not valid JSON")
        );
        assert_eq!(
            parse_error("[1,]").as_deref(),
            Some("Unexpected token ']', \"[1,]\" is not valid JSON")
        );
        assert_eq!(
            parse_error("{\"a\":1}\n x").as_deref(),
            Some("Unexpected non-whitespace character after JSON at position 9 (line 2 column 2)")
        );
    }
}
