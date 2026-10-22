//! Port of the npm `jsonrepair` 3.14.0 repairer (the version installed in the
//! fixed baseline, used by `providers/anthropic/stream.ts` for tool-call
//! arguments). Written from a reading of its algorithm; works on UTF-16 code
//! units like the JS original.
//!
//! The JS recursion (value → object/array → value) is bounded here by
//! [`MAX_DEPTH`]: deeper input fails (as an over-deep input makes the JS
//! version throw `RangeError`). Known divergence: V8's limit depends on the
//! stack and is larger, so inputs nested between the two limits repair in TS
//! but fail here. Ledger: specs/gateway-parity/accepted-diffs.md
//! A6.4-JSONREPAIR-DEPTH-001 (unapproved).
use pilotdeck_protocol::{JsString, JsValue, stringify_json};

/// Nesting levels accepted before giving up (keeps the Rust stack bounded
/// even on a 2 MiB async worker in a debug build).
pub(crate) const MAX_DEPTH: usize = 200;

/// `JSONRepairError` (message and position are not observable to callers).
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct RepairError(pub &'static str);

type Repair<T> = Result<T, RepairError>;

const SPACE: u16 = 0x20;
const NEWLINE: u16 = 0x0a;
const TAB: u16 = 0x09;
const RETURN: u16 = 0x0d;

fn units(text: &str) -> Vec<u16> {
    text.encode_utf16().collect()
}
fn is_ascii(c: Option<u16>, set: &str) -> bool {
    c.is_some_and(|c| c < 0x80 && set.as_bytes().contains(&(c as u8)))
}
fn is_digit(c: Option<u16>) -> bool {
    c.is_some_and(|c| (0x30..=0x39).contains(&c))
}
fn is_hex(c: Option<u16>) -> bool {
    c.is_some_and(|c| (c as u8 as u16 == c) && (c as u8).is_ascii_hexdigit())
}
fn is_delimiter(c: Option<u16>) -> bool {
    is_ascii(c, ",:[]/{}()\n+")
}
fn is_unquoted_string_delimiter(c: Option<u16>) -> bool {
    is_ascii(c, ",[]/{}\n+")
}
fn is_function_name_char_start(c: Option<u16>) -> bool {
    c.is_some_and(|c| c < 0x80 && ((c as u8).is_ascii_alphabetic() || c == 0x5f || c == 0x24))
}
fn is_function_name_char(c: Option<u16>) -> bool {
    is_function_name_char_start(c) || is_digit(c)
}
fn is_double_quote_like(c: Option<u16>) -> bool {
    matches!(c, Some(0x22 | 0x201c | 0x201d))
}
fn is_single_quote_like(c: Option<u16>) -> bool {
    matches!(c, Some(0x27 | 0x2018 | 0x2019 | 0x60 | 0xb4))
}
fn is_quote(c: Option<u16>) -> bool {
    is_double_quote_like(c) || is_single_quote_like(c)
}
fn is_start_of_value(c: Option<u16>) -> bool {
    // isQuote || /^[[{\w-]$/ (ASCII \w)
    is_quote(c)
        || c.is_some_and(|c| {
            c < 0x80 && ((c as u8).is_ascii_alphanumeric() || b"[{_-".contains(&(c as u8)))
        })
}
fn is_control_character(c: u16) -> bool {
    matches!(c, 0x0a | 0x0d | 0x09 | 0x08 | 0x0c)
}
fn control_escape(c: u16) -> &'static str {
    match c {
        0x08 => "\\b",
        0x0c => "\\f",
        0x0a => "\\n",
        0x0d => "\\r",
        _ => "\\t",
    }
}
fn is_whitespace_unit(c: Option<u16>) -> bool {
    matches!(c, Some(SPACE | NEWLINE | TAB | RETURN))
}
fn is_whitespace_except_newline(c: Option<u16>) -> bool {
    matches!(c, Some(SPACE | TAB | RETURN))
}
fn is_special_whitespace(c: Option<u16>) -> bool {
    matches!(
        c,
        Some(0xa0 | 0x180e | 0x2000..=0x200b | 0x202f | 0x205f | 0x3000 | 0xfeff)
    )
}
fn is_url_char(c: Option<u16>) -> bool {
    c.is_some_and(|c| {
        c < 0x80
            && ((c as u8).is_ascii_alphanumeric() || b"-._~:/?#@!$&'()*+;=".contains(&(c as u8)))
    })
}
fn is_url_start(segment: &[u16]) -> bool {
    [
        "http://",
        "https://",
        "ftp://",
        "mailto://",
        "file://",
        "data://",
        "irc://",
    ]
    .iter()
    .any(|s| segment == units(s).as_slice())
}

/// `insertBeforeLastWhitespace`.
fn insert_before_last_whitespace(text: &mut Vec<u16>, insert: &str) {
    let mut index = text.len();
    while index > 0 && is_whitespace_unit(Some(text[index - 1])) {
        index -= 1;
    }
    let tail = text.split_off(index);
    text.extend(insert.encode_utf16());
    text.extend(tail);
}
/// `stripLastOccurrence` for a one-unit needle.
fn strip_last_occurrence(text: &mut Vec<u16>, needle: u16, strip_remaining: bool) {
    if let Some(index) = text.iter().rposition(|c| *c == needle) {
        if strip_remaining {
            text.truncate(index);
        } else {
            text.remove(index);
        }
    }
}
/// `/[,\n][ \t\r]*$/`.
fn ends_with_comma_or_newline(text: &[u16]) -> bool {
    let trimmed = text
        .iter()
        .rposition(|c| !matches!(*c, SPACE | TAB | RETURN))
        .map(|i| text[i]);
    matches!(trimmed, Some(0x2c | NEWLINE))
}

#[derive(Clone, Copy)]
enum EndQuote {
    Double,
    Single,
    SingleLike,
    DoubleLike,
}
impl EndQuote {
    fn matches(self, c: Option<u16>) -> bool {
        match self {
            Self::Double => c == Some(0x22),
            Self::Single => c == Some(0x27),
            Self::SingleLike => is_single_quote_like(c),
            Self::DoubleLike => is_double_quote_like(c),
        }
    }
}

struct Repairer<'a> {
    text: &'a [u16],
    i: usize,
    output: Vec<u16>,
    depth: usize,
}

/// `jsonrepair(text)`.
pub(crate) fn jsonrepair(text: &[u16]) -> Repair<Vec<u16>> {
    let mut r = Repairer {
        text,
        i: 0,
        output: Vec::new(),
        depth: 0,
    };
    r.run()?;
    Ok(r.output)
}

impl Repairer<'_> {
    fn at(&self, index: usize) -> Option<u16> {
        self.text.get(index).copied()
    }
    fn cur(&self) -> Option<u16> {
        self.at(self.i)
    }
    fn len(&self) -> usize {
        self.text.len()
    }
    fn ws_at(&self, index: usize) -> bool {
        is_whitespace_unit(self.at(index))
    }
    fn push(&mut self, text: &str) {
        self.output.extend(text.encode_utf16());
    }

    fn run(&mut self) -> Repair<()> {
        self.parse_markdown_code_block(&["```", "[```", "{```"]);
        if !self.parse_value()? {
            return Err(RepairError("Unexpected end of json string"));
        }
        self.parse_markdown_code_block(&["```", "```]", "```}"]);
        let processed_comma = self.parse_character(0x2c);
        if processed_comma {
            self.parse_whitespace_and_skip_comments(true);
        }
        if is_start_of_value(self.cur()) && ends_with_comma_or_newline(&self.output) {
            if !processed_comma {
                insert_before_last_whitespace(&mut self.output, ",");
            }
            self.parse_newline_delimited_json()?;
        } else if processed_comma {
            strip_last_occurrence(&mut self.output, 0x2c, false);
        }
        while matches!(self.cur(), Some(0x7d | 0x5d)) {
            self.i += 1;
            self.parse_whitespace_and_skip_comments(true);
        }
        if self.i >= self.len() {
            return Ok(());
        }
        Err(RepairError("Unexpected character"))
    }

    fn parse_value(&mut self) -> Repair<bool> {
        if self.depth >= MAX_DEPTH {
            return Err(RepairError("Maximum call stack size exceeded"));
        }
        self.depth += 1;
        self.parse_whitespace_and_skip_comments(true);
        let processed = self.parse_object()?
            || self.parse_array()?
            || self.parse_string(false, None)?
            || self.parse_number()
            || self.parse_keywords()
            || self.parse_unquoted_string(false)?
            || self.parse_regex();
        self.parse_whitespace_and_skip_comments(true);
        self.depth -= 1;
        Ok(processed)
    }

    fn parse_whitespace_and_skip_comments(&mut self, skip_newline: bool) -> bool {
        let start = self.i;
        self.parse_whitespace(skip_newline);
        loop {
            if !self.parse_comment() {
                break;
            }
            if !self.parse_whitespace(skip_newline) {
                break;
            }
        }
        self.i > start
    }

    fn parse_whitespace(&mut self, skip_newline: bool) -> bool {
        let start = self.output.len();
        loop {
            let c = self.cur();
            let plain = if skip_newline {
                is_whitespace_unit(c)
            } else {
                is_whitespace_except_newline(c)
            };
            if plain {
                self.output.push(c.expect("whitespace"));
                self.i += 1;
            } else if is_special_whitespace(c) {
                self.output.push(SPACE);
                self.i += 1;
            } else {
                break;
            }
        }
        self.output.len() > start
    }

    fn parse_comment(&mut self) -> bool {
        if self.cur() == Some(0x2f) && self.at(self.i + 1) == Some(0x2a) {
            while self.i < self.len()
                && !(self.cur() == Some(0x2a) && self.at(self.i + 1) == Some(0x2f))
            {
                self.i += 1;
            }
            self.i += 2;
            return true;
        }
        if self.cur() == Some(0x2f) && self.at(self.i + 1) == Some(0x2f) {
            while self.i < self.len() && self.cur() != Some(NEWLINE) {
                self.i += 1;
            }
            return true;
        }
        false
    }

    fn parse_markdown_code_block(&mut self, blocks: &[&str]) -> bool {
        self.parse_whitespace(true);
        let mut matched = false;
        for block in blocks {
            let block = units(block);
            let end = self.i + block.len();
            if self.text.get(self.i..end) == Some(block.as_slice()) {
                self.i = end;
                matched = true;
                break;
            }
        }
        if !matched {
            return false;
        }
        if is_function_name_char_start(self.cur()) {
            while self.i < self.len() && is_function_name_char(self.cur()) {
                self.i += 1;
            }
        }
        self.parse_whitespace_and_skip_comments(true);
        true
    }

    fn parse_character(&mut self, c: u16) -> bool {
        if self.cur() == Some(c) {
            self.output.push(c);
            self.i += 1;
            return true;
        }
        false
    }
    fn skip_character(&mut self, c: u16) -> bool {
        if self.cur() == Some(c) {
            self.i += 1;
            return true;
        }
        false
    }

    fn skip_ellipsis(&mut self) {
        self.parse_whitespace_and_skip_comments(true);
        if self.cur() == Some(0x2e)
            && self.at(self.i + 1) == Some(0x2e)
            && self.at(self.i + 2) == Some(0x2e)
        {
            self.i += 3;
            self.parse_whitespace_and_skip_comments(true);
            self.skip_character(0x2c);
        }
    }

    fn parse_object(&mut self) -> Repair<bool> {
        if self.cur() != Some(0x7b) {
            return Ok(false);
        }
        self.push("{");
        self.i += 1;
        self.parse_whitespace_and_skip_comments(true);
        if self.skip_character(0x2c) {
            self.parse_whitespace_and_skip_comments(true);
        }
        let mut initial = true;
        while self.i < self.len() && self.cur() != Some(0x7d) {
            if !initial {
                if !self.parse_character(0x2c) {
                    insert_before_last_whitespace(&mut self.output, ",");
                }
                self.parse_whitespace_and_skip_comments(true);
            } else {
                initial = false;
            }
            self.skip_ellipsis();
            let key = self.parse_string(false, None)? || self.parse_unquoted_string(true)?;
            if !key {
                if matches!(self.cur(), Some(0x7d | 0x7b | 0x5d | 0x5b) | None) {
                    strip_last_occurrence(&mut self.output, 0x2c, false);
                } else {
                    return Err(RepairError("Object key expected"));
                }
                break;
            }
            self.parse_whitespace_and_skip_comments(true);
            let colon = self.parse_character(0x3a);
            let truncated = self.i >= self.len();
            if !colon {
                if is_start_of_value(self.cur()) || truncated {
                    insert_before_last_whitespace(&mut self.output, ":");
                } else {
                    return Err(RepairError("Colon expected"));
                }
            }
            if !self.parse_value()? {
                if colon || truncated {
                    self.push("null");
                } else {
                    return Err(RepairError("Colon expected"));
                }
            }
        }
        if self.cur() == Some(0x7d) {
            self.push("}");
            self.i += 1;
        } else {
            insert_before_last_whitespace(&mut self.output, "}");
        }
        Ok(true)
    }

    fn parse_array(&mut self) -> Repair<bool> {
        if self.cur() != Some(0x5b) {
            return Ok(false);
        }
        self.push("[");
        self.i += 1;
        self.parse_whitespace_and_skip_comments(true);
        if self.skip_character(0x2c) {
            self.parse_whitespace_and_skip_comments(true);
        }
        let mut initial = true;
        while self.i < self.len() && self.cur() != Some(0x5d) {
            if !initial {
                if !self.parse_character(0x2c) {
                    insert_before_last_whitespace(&mut self.output, ",");
                }
            } else {
                initial = false;
            }
            self.skip_ellipsis();
            if !self.parse_value()? {
                strip_last_occurrence(&mut self.output, 0x2c, false);
                break;
            }
        }
        if self.cur() == Some(0x5d) {
            self.push("]");
            self.i += 1;
        } else {
            insert_before_last_whitespace(&mut self.output, "]");
        }
        Ok(true)
    }

    fn parse_newline_delimited_json(&mut self) -> Repair<()> {
        let mut initial = true;
        loop {
            if !initial {
                if !self.parse_character(0x2c) {
                    insert_before_last_whitespace(&mut self.output, ",");
                }
            } else {
                initial = false;
            }
            if !self.parse_value()? {
                break;
            }
        }
        strip_last_occurrence(&mut self.output, 0x2c, false);
        let mut wrapped = units("[\n");
        wrapped.append(&mut self.output);
        wrapped.extend(units("\n]"));
        self.output = wrapped;
        Ok(())
    }

    fn prev_non_whitespace_index(&self, start: usize) -> usize {
        let mut prev = start;
        while prev > 0 && self.ws_at(prev) {
            prev -= 1;
        }
        prev
    }

    /// `parseString(stopAtDelimiter, stopAtIndex)`.
    fn parse_string(
        &mut self,
        stop_at_delimiter: bool,
        stop_at_index: Option<usize>,
    ) -> Repair<bool> {
        let skip_escape_chars = self.cur() == Some(0x5c);
        if skip_escape_chars {
            self.i += 1;
        }
        let first = self.cur();
        if !is_quote(first) {
            return Ok(false);
        }
        let end_quote = if first == Some(0x22) {
            EndQuote::Double
        } else if first == Some(0x27) {
            EndQuote::Single
        } else if is_single_quote_like(first) {
            EndQuote::SingleLike
        } else {
            EndQuote::DoubleLike
        };
        let i_before = self.i;
        let o_before = self.output.len();
        let mut s: Vec<u16> = vec![0x22];
        self.i += 1;
        loop {
            if self.i >= self.len() {
                let i_prev = self.prev_non_whitespace_index(self.i - 1);
                // `text.charAt(past the end)` is "", and `",:[]…".includes("")`
                // is true (reachable after a trailing backslash escape).
                let prev_is_delimiter = i_prev >= self.len() || is_delimiter(self.at(i_prev));
                if !stop_at_delimiter && prev_is_delimiter {
                    self.i = i_before;
                    self.output.truncate(o_before);
                    return self.parse_string(true, None);
                }
                insert_before_last_whitespace(&mut s, "\"");
                self.output.extend(s);
                return Ok(true);
            }
            if Some(self.i) == stop_at_index {
                insert_before_last_whitespace(&mut s, "\"");
                self.output.extend(s);
                return Ok(true);
            }
            let c = self.cur();
            if end_quote.matches(c) {
                let i_quote = self.i;
                let o_quote = s.len();
                s.push(0x22);
                self.i += 1;
                self.output.extend_from_slice(&s);
                self.parse_whitespace_and_skip_comments(false);
                let next = self.cur();
                if stop_at_delimiter
                    || self.i >= self.len()
                    || is_delimiter(next)
                    || is_quote(next)
                    || is_digit(next)
                {
                    self.parse_concatenated_string()?;
                    return Ok(true);
                }
                let i_prev_char = self.prev_non_whitespace_index(i_quote - 1);
                let prev_char = self.at(i_prev_char);
                if prev_char == Some(0x2c) {
                    self.i = i_before;
                    self.output.truncate(o_before);
                    return self.parse_string(false, Some(i_prev_char));
                }
                if is_delimiter(prev_char) {
                    self.i = i_before;
                    self.output.truncate(o_before);
                    return self.parse_string(true, None);
                }
                self.output.truncate(o_before);
                self.i = i_quote + 1;
                s.insert(o_quote, 0x5c);
            } else if stop_at_delimiter && is_unquoted_string_delimiter(c) {
                if self.i > 0
                    && self.at(self.i - 1) == Some(0x3a)
                    && is_url_start(&self.text[i_before + 1..(self.i + 2).min(self.len())])
                {
                    while self.i < self.len() && is_url_char(self.cur()) {
                        s.push(self.cur().expect("in range"));
                        self.i += 1;
                    }
                }
                insert_before_last_whitespace(&mut s, "\"");
                self.output.extend(s);
                self.parse_concatenated_string()?;
                return Ok(true);
            } else if c == Some(0x5c) {
                let next = self.at(self.i + 1);
                match next {
                    Some(0x22 | 0x5c | 0x2f | 0x62 | 0x66 | 0x6e | 0x72 | 0x74) => {
                        s.extend_from_slice(&self.text[self.i..self.i + 2]);
                        self.i += 2;
                    }
                    Some(0x75) => {
                        let mut j = 2;
                        while j < 6 && is_hex(self.at(self.i + j)) {
                            j += 1;
                        }
                        if j == 6 {
                            s.extend_from_slice(&self.text[self.i..self.i + 6]);
                            self.i += 6;
                        } else if self.i + j >= self.len() {
                            self.i = self.len();
                        } else {
                            return Err(RepairError("Invalid unicode character"));
                        }
                    }
                    Some(NEWLINE) => {
                        s.extend(units("\\n"));
                        self.i += 2;
                    }
                    other => {
                        // Drop the backslash; `text.charAt` past the end is "".
                        if let Some(other) = other {
                            s.push(other);
                        }
                        self.i += 2;
                    }
                }
            } else {
                let c = c.expect("in range");
                if c == 0x22 && self.at(self.i - 1) != Some(0x5c) {
                    s.extend(units("\\\""));
                    self.i += 1;
                } else if is_control_character(c) {
                    s.extend(units(control_escape(c)));
                    self.i += 1;
                } else {
                    if c < 0x20 {
                        return Err(RepairError("Invalid character"));
                    }
                    s.push(c);
                    self.i += 1;
                }
            }
            if skip_escape_chars {
                self.skip_character(0x5c);
            }
        }
    }

    fn parse_concatenated_string(&mut self) -> Repair<bool> {
        let mut processed = false;
        self.parse_whitespace_and_skip_comments(true);
        while self.cur() == Some(0x2b) {
            processed = true;
            self.i += 1;
            self.parse_whitespace_and_skip_comments(true);
            strip_last_occurrence(&mut self.output, 0x22, true);
            let start = self.output.len();
            if self.parse_string(false, None)? {
                if start < self.output.len() {
                    self.output.remove(start);
                }
            } else {
                insert_before_last_whitespace(&mut self.output, "\"");
            }
        }
        Ok(processed)
    }

    fn at_end_of_number(&self) -> bool {
        self.i >= self.len() || is_delimiter(self.cur()) || self.ws_at(self.i)
    }
    fn repair_number_ending_with_numeric_symbol(&mut self, start: usize) {
        let end = self.i.min(self.len());
        self.output.extend_from_slice(&self.text[start..end]);
        self.output.push(0x30);
    }

    fn parse_number(&mut self) -> bool {
        let start = self.i;
        if self.cur() == Some(0x2d) {
            self.i += 1;
            if self.at_end_of_number() {
                self.repair_number_ending_with_numeric_symbol(start);
                return true;
            }
            if !is_digit(self.cur()) {
                self.i = start;
                return false;
            }
        }
        while is_digit(self.cur()) {
            self.i += 1;
        }
        if self.cur() == Some(0x2e) {
            self.i += 1;
            if self.at_end_of_number() {
                self.repair_number_ending_with_numeric_symbol(start);
                return true;
            }
            if !is_digit(self.cur()) {
                self.i = start;
                return false;
            }
            while is_digit(self.cur()) {
                self.i += 1;
            }
        }
        if matches!(self.cur(), Some(0x65 | 0x45)) {
            self.i += 1;
            if matches!(self.cur(), Some(0x2d | 0x2b)) {
                self.i += 1;
            }
            if self.at_end_of_number() {
                self.repair_number_ending_with_numeric_symbol(start);
                return true;
            }
            if !is_digit(self.cur()) {
                self.i = start;
                return false;
            }
            while is_digit(self.cur()) {
                self.i += 1;
            }
        }
        if !self.at_end_of_number() {
            self.i = start;
            return false;
        }
        if self.i > start {
            let num = &self.text[start..self.i];
            let leading_zero = num.len() >= 2 && num[0] == 0x30 && is_digit(Some(num[1]));
            if leading_zero {
                self.output.push(0x22);
                self.output.extend_from_slice(num);
                self.output.push(0x22);
            } else {
                self.output.extend_from_slice(num);
            }
            return true;
        }
        false
    }

    fn parse_keywords(&mut self) -> bool {
        [
            ("true", "true"),
            ("false", "false"),
            ("null", "null"),
            ("True", "true"),
            ("False", "false"),
            ("None", "null"),
        ]
        .iter()
        .any(|(name, value)| {
            let name = units(name);
            if self.text.get(self.i..self.i + name.len()) == Some(name.as_slice()) {
                self.push(value);
                self.i += name.len();
                true
            } else {
                false
            }
        })
    }

    fn parse_unquoted_string(&mut self, is_key: bool) -> Repair<bool> {
        let start = self.i;
        if is_function_name_char_start(self.cur()) {
            while self.i < self.len() && is_function_name_char(self.cur()) {
                self.i += 1;
            }
            let mut j = self.i;
            while self.ws_at(j) {
                j += 1;
            }
            if self.at(j) == Some(0x28) {
                self.i = j + 1;
                self.parse_value()?;
                if self.cur() == Some(0x29) {
                    self.i += 1;
                    if self.cur() == Some(0x3b) {
                        self.i += 1;
                    }
                }
                return Ok(true);
            }
        }
        while self.i < self.len()
            && !is_unquoted_string_delimiter(self.cur())
            && !is_quote(self.cur())
            && (!is_key || self.cur() != Some(0x3a))
        {
            self.i += 1;
        }
        if self.i > 0
            && self.at(self.i - 1) == Some(0x3a)
            && is_url_start(&self.text[start..(self.i + 2).min(self.len())])
        {
            while self.i < self.len() && is_url_char(self.cur()) {
                self.i += 1;
            }
        }
        if self.i > start {
            while self.i > 0 && self.ws_at(self.i - 1) {
                self.i -= 1;
            }
            let symbol = &self.text[start..self.i];
            if symbol == units("undefined").as_slice() {
                self.push("null");
            } else {
                self.push(&json_string(symbol));
            }
            if self.cur() == Some(0x22) {
                self.i += 1;
            }
            return Ok(true);
        }
        Ok(false)
    }

    fn parse_regex(&mut self) -> bool {
        if self.cur() != Some(0x2f) {
            return false;
        }
        let start = self.i;
        self.i += 1;
        while self.i < self.len() && (self.cur() != Some(0x2f) || self.at(self.i - 1) == Some(0x5c))
        {
            self.i += 1;
        }
        self.i += 1;
        let end = self.i.min(self.len());
        let regex = json_string(&self.text[start..end]);
        self.push(&regex);
        true
    }
}

/// `JSON.stringify(string)` (lone surrogates escaped, like V8).
fn json_string(units: &[u16]) -> String {
    stringify_json(&JsValue::String(JsString::from_utf16(units.to_vec())))
        .ok()
        .flatten()
        .unwrap_or_default()
}

#[cfg(test)]
#[path = "jsonrepair_tests.rs"]
mod replay;

#[cfg(test)]
mod tests {
    use super::*;

    fn repair(text: &str) -> Result<String, RepairError> {
        jsonrepair(&units(text)).map(|out| String::from_utf16(&out).unwrap())
    }

    #[test]
    fn depth_limit_fails_instead_of_overflowing_the_stack() {
        let deep = "[".repeat(MAX_DEPTH + 5);
        assert!(repair(&deep).is_err());
        let ok = "[".repeat(MAX_DEPTH - 1);
        assert_eq!(
            repair(&ok).unwrap(),
            format!("{ok}{}", "]".repeat(MAX_DEPTH - 1))
        );
        // Runs within a 2 MiB thread (tokio's default worker stack).
        let handle = std::thread::Builder::new()
            .stack_size(2 << 20)
            .spawn(move || {
                let nested = "{\"a\":[".repeat(MAX_DEPTH / 2 - 1);
                repair(&nested).is_ok()
            })
            .unwrap();
        assert!(handle.join().unwrap());
    }
}
