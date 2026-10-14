// Port of yaml@2.9.0 dist/parse/lexer.js (ISC).
// State names, transitions and token boundaries follow the upstream generator.
// Vec output replaces yield; all indices are UTF-16 code-unit indices.
use pilotdeck_protocol::JsString;

#[derive(Clone, Copy, Debug)]
enum State {
    Stream,
    LineStart,
    BlockStart,
    Doc,
    Flow,
    QuotedScalar,
    BlockScalar,
    PlainScalar,
}
use State::*;

fn is_empty(ch: Option<u16>) -> bool {
    matches!(ch, None | Some(32 | 10 | 13 | 9))
}
fn flow_indicator(ch: Option<u16>) -> bool {
    matches!(ch, Some(44 | 91 | 93 | 123 | 125))
}
fn not_anchor(ch: Option<u16>) -> bool {
    is_empty(ch) || flow_indicator(ch)
}
fn hex(ch: Option<u16>) -> bool {
    matches!(ch, Some(48..=57 | 65..=70 | 97..=102))
}
fn tag_char(ch: u16) -> bool {
    matches!(ch, 48..=57 | 65..=90 | 97..=122)
        || b"-#;/?:@&=+$_.!~*'()".iter().any(|c| u16::from(*c) == ch)
}

#[derive(Debug)]
pub struct Lexer {
    at_end: bool,
    block_scalar_indent: isize,
    block_scalar_keep: bool,
    buffer: Vec<u16>,
    flow_key: bool,
    flow_level: usize,
    indent_next: usize,
    indent_value: usize,
    line_end_pos: Option<isize>,
    next: Option<State>,
    pos: usize,
    output: Vec<JsString>,
}
impl Default for Lexer {
    fn default() -> Self {
        Self {
            at_end: false,
            block_scalar_indent: -1,
            block_scalar_keep: false,
            buffer: Vec::new(),
            flow_key: false,
            flow_level: 0,
            indent_next: 0,
            indent_value: 0,
            line_end_pos: None,
            next: None,
            pos: 0,
            output: Vec::new(),
        }
    }
}
impl Lexer {
    pub fn lex(&mut self, source: &JsString, incomplete: bool) -> Vec<JsString> {
        if !source.is_empty() {
            self.buffer.extend_from_slice(source.units());
            self.line_end_pos = None;
        }
        self.at_end = !incomplete;
        let mut next = Some(self.next.unwrap_or(Stream));
        while let Some(state) = next {
            if !incomplete && !self.has_chars(1) {
                break;
            }
            next = self.parse_next(state);
        }
        std::mem::take(&mut self.output)
    }
    fn at(&self, i: isize) -> Option<u16> {
        usize::try_from(i)
            .ok()
            .and_then(|i| self.buffer.get(i).copied())
    }
    fn char_at(&self, n: usize) -> Option<u16> {
        self.buffer.get(self.pos + n).copied()
    }
    fn index_of(&self, ch: u16, from: usize) -> Option<usize> {
        self.buffer
            .get(from..)?
            .iter()
            .position(|c| *c == ch)
            .map(|i| i + from)
    }
    fn at_line_end(&self) -> bool {
        let mut i = self.pos;
        while matches!(self.buffer.get(i), Some(32 | 9)) {
            i += 1;
        }
        match self.buffer.get(i) {
            None | Some(35 | 10) => true,
            Some(13) => self.buffer.get(i + 1) == Some(&10),
            _ => false,
        }
    }
    fn continue_scalar(&self, offset: usize) -> Option<usize> {
        let mut ch = self.buffer.get(offset).copied();
        if self.indent_next > 0 {
            let mut indent = 0;
            while ch == Some(32) {
                indent += 1;
                ch = self.buffer.get(offset + indent).copied();
            }
            if ch == Some(13) {
                let next = self.buffer.get(offset + indent + 1);
                if next == Some(&10) || (next.is_none() && !self.at_end) {
                    return Some(offset + indent + 1);
                }
            }
            return if ch == Some(10) || indent >= self.indent_next || (ch.is_none() && !self.at_end)
            {
                Some(offset + indent)
            } else {
                None
            };
        }
        if matches!(ch, Some(45 | 46)) && self.doc_marker(offset) {
            return None;
        }
        Some(offset)
    }
    fn doc_marker(&self, offset: usize) -> bool {
        (self.buffer.get(offset..offset + 3) == Some(&[45, 45, 45])
            || self.buffer.get(offset..offset + 3) == Some(&[46, 46, 46]))
            && is_empty(self.buffer.get(offset + 3).copied())
    }
    fn get_line(&mut self) -> Option<std::ops::Range<usize>> {
        let end = match self.line_end_pos {
            Some(end) if end == -1 || end >= self.pos as isize => end,
            _ => {
                let end = self.index_of(10, self.pos).map_or(-1, |i| i as isize);
                self.line_end_pos = Some(end);
                end
            }
        };
        if end == -1 {
            return self.at_end.then_some(self.pos..self.buffer.len());
        }
        let end = if self.at(end - 1) == Some(13) {
            end - 1
        } else {
            end
        };
        Some(self.pos..end as usize)
    }
    fn has_chars(&self, n: usize) -> bool {
        self.pos + n <= self.buffer.len()
    }
    fn set_next(&mut self, state: State) -> Option<State> {
        self.buffer.drain(..self.pos);
        self.pos = 0;
        self.line_end_pos = None;
        self.next = Some(state);
        None
    }
    fn emit_control(&mut self, ch: u16) {
        self.output.push(JsString::from_utf16(vec![ch]));
    }
    fn parse_next(&mut self, state: State) -> Option<State> {
        match state {
            Stream => self.parse_stream(),
            LineStart => self.parse_line_start(),
            BlockStart => self.parse_block_start(),
            Doc => self.parse_document(),
            Flow => self.parse_flow_collection(),
            QuotedScalar => self.parse_quoted_scalar(),
            BlockScalar => self.parse_block_scalar(),
            PlainScalar => self.parse_plain_scalar(),
        }
    }
    fn parse_stream(&mut self) -> Option<State> {
        let Some(mut line) = self.get_line() else {
            return self.set_next(Stream);
        };
        if self.buffer.get(line.start) == Some(&0xfeff) {
            self.push_count(1);
            line.start += 1;
        }
        if self.buffer.get(line.start) == Some(&37) {
            let mut dir_end = line.len();
            for (cs, ch) in self.buffer[line.clone()].iter().enumerate() {
                if *ch == 35 && cs > 0 && matches!(self.buffer[line.start + cs - 1], 32 | 9) {
                    dir_end = cs - 1;
                    break;
                }
            }
            while dir_end > 0 && matches!(self.buffer[line.start + dir_end - 1], 32 | 9) {
                dir_end -= 1;
            }
            let n = self.push_count(dir_end) + self.push_spaces(true);
            self.push_count(line.len() - n);
            // Upstream calls the generator without yielding it here: no newline consumed.
            return Some(Stream);
        }
        if self.at_line_end() {
            let sp = self.push_spaces(true);
            self.push_count(line.len() - sp);
            self.push_newline();
            return Some(Stream);
        }
        self.emit_control(2);
        Some(LineStart)
    }
    fn parse_line_start(&mut self) -> Option<State> {
        let ch = self.char_at(0);
        if ch.is_none() && !self.at_end {
            return self.set_next(LineStart);
        }
        if matches!(ch, Some(45 | 46)) {
            if !self.at_end && !self.has_chars(4) {
                return self.set_next(LineStart);
            }
            if self.doc_marker(self.pos) {
                self.push_count(3);
                self.indent_value = 0;
                self.indent_next = 0;
                return Some(if ch == Some(45) { Doc } else { Stream });
            }
        }
        self.indent_value = self.push_spaces(false);
        if self.indent_next > self.indent_value && !is_empty(self.char_at(1)) {
            self.indent_next = self.indent_value;
        }
        Some(BlockStart)
    }
    fn parse_block_start(&mut self) -> Option<State> {
        let ch0 = self.char_at(0);
        // JS destructures [...peek(2)] by code point, even though peek slices
        // two UTF-16 units. A leading surrogate pair therefore has no ch1 and
        // buffers incomplete input until the final flush (including lineStarts).
        let ch1 = if matches!(ch0, Some(0xd800..=0xdbff))
            && matches!(self.char_at(1), Some(0xdc00..=0xdfff))
        {
            None
        } else {
            self.char_at(1)
        };
        if ch1.is_none() && !self.at_end {
            return self.set_next(BlockStart);
        }
        if matches!(ch0, Some(45 | 63 | 58)) && is_empty(ch1) {
            let n = self.push_count(1) + self.push_spaces(true);
            self.indent_next = self.indent_value + 1;
            self.indent_value += n;
            return Some(BlockStart);
        }
        Some(Doc)
    }
    fn parse_document(&mut self) -> Option<State> {
        self.push_spaces(true);
        let Some(line) = self.get_line() else {
            return self.set_next(Doc);
        };
        let mut n = self.push_indicators();
        match self
            .buffer
            .get(line.start + n)
            .filter(|_| line.start + n < line.end)
        {
            Some(35) | None => {
                if self
                    .buffer
                    .get(line.start + n)
                    .filter(|_| line.start + n < line.end)
                    == Some(&35)
                {
                    self.push_count(line.len() - n);
                }
                self.push_newline();
                Some(LineStart)
            }
            Some(123 | 91) => {
                self.push_count(1);
                self.flow_key = false;
                self.flow_level = 1;
                Some(Flow)
            }
            Some(125 | 93) => {
                self.push_count(1);
                Some(Doc)
            }
            Some(42) => {
                self.push_until(not_anchor);
                Some(Doc)
            }
            Some(34 | 39) => Some(QuotedScalar),
            Some(124 | 62) => {
                n += self.parse_block_scalar_header();
                n += self.push_spaces(true);
                self.push_count(line.len() - n);
                self.push_newline();
                Some(BlockScalar)
            }
            _ => Some(PlainScalar),
        }
    }
    fn parse_flow_collection(&mut self) -> Option<State> {
        let mut indent = None;
        loop {
            let nl = self.push_newline();
            let mut sp = if nl > 0 {
                let sp = self.push_spaces(false);
                self.indent_value = sp;
                indent = Some(sp);
                sp
            } else {
                0
            };
            sp += self.push_spaces(true);
            if nl + sp == 0 {
                break;
            }
        }
        let Some(line) = self.get_line() else {
            return self.set_next(Flow);
        };
        if (indent.is_some_and(|i| i < self.indent_next)
            && self.buffer.get(line.start) != Some(&35))
            || (indent == Some(0) && self.doc_marker(self.pos))
        {
            let at_flow_end_marker = self.indent_next.checked_sub(1) == indent
                && self.flow_level == 1
                && matches!(self.buffer.get(line.start), Some(93 | 125));
            if !at_flow_end_marker {
                self.flow_level = 0;
                self.emit_control(24);
                return Some(LineStart);
            }
        }
        let mut n = 0;
        while self
            .buffer
            .get(line.start + n)
            .filter(|_| line.start + n < line.end)
            == Some(&44)
        {
            n += self.push_count(1);
            n += self.push_spaces(true);
            self.flow_key = false;
        }
        n += self.push_indicators();
        match self
            .buffer
            .get(line.start + n)
            .filter(|_| line.start + n < line.end)
        {
            None => Some(Flow),
            Some(35) => {
                self.push_count(line.len() - n);
                Some(Flow)
            }
            Some(123 | 91) => {
                self.push_count(1);
                self.flow_key = false;
                self.flow_level += 1;
                Some(Flow)
            }
            Some(125 | 93) => {
                self.push_count(1);
                self.flow_key = true;
                self.flow_level -= 1;
                Some(if self.flow_level > 0 { Flow } else { Doc })
            }
            Some(42) => {
                self.push_until(not_anchor);
                Some(Flow)
            }
            Some(34 | 39) => {
                self.flow_key = true;
                Some(QuotedScalar)
            }
            Some(58)
                if self.flow_key || is_empty(self.char_at(1)) || self.char_at(1) == Some(44) =>
            {
                self.flow_key = false;
                self.push_count(1);
                self.push_spaces(true);
                Some(Flow)
            }
            _ => {
                self.flow_key = false;
                Some(PlainScalar)
            }
        }
    }
    fn parse_quoted_scalar(&mut self) -> Option<State> {
        let quote = self.char_at(0).unwrap();
        let mut end = self.index_of(quote, self.pos + 1);
        if quote == 39 {
            while let Some(e) = end {
                if self.buffer.get(e + 1) != Some(&39) {
                    break;
                }
                end = self.index_of(39, e + 2);
            }
        } else {
            while let Some(e) = end {
                let mut n = 0;
                while self.at(e as isize - 1 - n) == Some(92) {
                    n += 1;
                }
                if n % 2 == 0 {
                    break;
                }
                end = self.index_of(34, e + 1);
            }
        }
        // JS substring(0, -1) is empty; don't inspect newlines if quote is absent.
        let quote_end = end.unwrap_or(0);
        let mut nl = self.index_of(10, self.pos).filter(|i| *i < quote_end);
        while let Some(n) = nl {
            let Some(cs) = self.continue_scalar(n + 1) else {
                break;
            };
            nl = self.index_of(10, cs).filter(|i| *i < quote_end);
        }
        if let Some(nl) = nl {
            end = Some(
                nl - if self.at(nl as isize - 1) == Some(13) {
                    2
                } else {
                    1
                },
            );
        }
        let end = match end {
            Some(e) => e,
            None if !self.at_end => return self.set_next(QuotedScalar),
            None => self.buffer.len(),
        };
        self.push_to_index(end + 1, false);
        Some(if self.flow_level > 0 { Flow } else { Doc })
    }
    fn parse_block_scalar_header(&mut self) -> usize {
        self.block_scalar_indent = -1;
        self.block_scalar_keep = false;
        let mut i = self.pos;
        loop {
            i += 1;
            match self.buffer.get(i) {
                Some(43) => self.block_scalar_keep = true,
                Some(ch @ 49..=57) => self.block_scalar_indent = (*ch - 49) as isize,
                Some(45) => (),
                _ => break,
            }
        }
        self.push_until(|ch| is_empty(ch) || ch == Some(35))
    }
    fn parse_block_scalar(&mut self) -> Option<State> {
        let mut nl = self.pos as isize - 1;
        let mut indent = 0;
        let mut i = self.pos;
        loop {
            match self.buffer.get(i) {
                Some(32) => indent += 1,
                Some(10) => {
                    nl = i as isize;
                    indent = 0;
                }
                Some(13) => {
                    let next = self.buffer.get(i + 1);
                    if next.is_none() && !self.at_end {
                        return self.set_next(BlockScalar);
                    }
                    if next != Some(&10) {
                        break;
                    }
                }
                _ => break,
            }
            i += 1;
        }
        if i >= self.buffer.len() && !self.at_end {
            return self.set_next(BlockScalar);
        }
        if indent >= self.indent_next {
            self.indent_next = if self.block_scalar_indent == -1 {
                indent
            } else {
                self.block_scalar_indent as usize + self.indent_next.max(1)
            };
            while let Some(cs) = self.continue_scalar((nl + 1) as usize) {
                nl = self.index_of(10, cs).map_or(-1, |i| i as isize);
                if nl == -1 {
                    break;
                }
            }
            if nl == -1 {
                if !self.at_end {
                    return self.set_next(BlockScalar);
                }
                nl = self.buffer.len() as isize;
            }
        }
        let mut i = nl + 1;
        while self.at(i) == Some(32) {
            i += 1;
        }
        if self.at(i) == Some(9) {
            while matches!(self.at(i), Some(9 | 32 | 13 | 10)) {
                i += 1;
            }
            nl = i - 1;
        } else if !self.block_scalar_keep {
            loop {
                let mut i = nl - 1;
                if self.at(i) == Some(13) {
                    i -= 1;
                }
                let last_char = i;
                while self.at(i) == Some(32) {
                    i -= 1;
                }
                if self.at(i) == Some(10)
                    && i >= self.pos as isize
                    && i + 1 + indent as isize > last_char
                {
                    nl = i;
                } else {
                    break;
                }
            }
        }
        self.emit_control(31);
        self.push_to_index((nl + 1) as usize, true);
        Some(LineStart)
    }
    fn parse_plain_scalar(&mut self) -> Option<State> {
        let in_flow = self.flow_level > 0;
        let mut end = self.pos as isize - 1;
        let mut i = self.pos as isize - 1;
        let ch = loop {
            i += 1;
            let Some(mut ch) = self.at(i) else {
                break None;
            };
            if ch == 58 {
                let next = self.at(i + 1);
                if is_empty(next) || (in_flow && flow_indicator(next)) {
                    break Some(ch);
                }
                end = i;
            } else if is_empty(Some(ch)) {
                let mut next = self.at(i + 1);
                if ch == 13 {
                    if next == Some(10) {
                        i += 1;
                        ch = 10;
                        next = self.at(i + 1);
                    } else {
                        end = i;
                    }
                }
                if next == Some(35) || (in_flow && flow_indicator(next)) {
                    break Some(ch);
                }
                if ch == 10 {
                    let Some(cs) = self.continue_scalar((i + 1) as usize) else {
                        break Some(ch);
                    };
                    i = i.max(cs as isize - 2);
                }
            } else {
                if in_flow && flow_indicator(Some(ch)) {
                    break Some(ch);
                }
                end = i;
            }
        };
        if ch.is_none() && !self.at_end {
            return self.set_next(PlainScalar);
        }
        self.emit_control(31);
        self.push_to_index((end + 1) as usize, true);
        Some(if in_flow { Flow } else { Doc })
    }
    fn push_count(&mut self, n: usize) -> usize {
        if n > 0 {
            self.output.push(JsString::from_utf16(
                self.buffer[self.pos..(self.pos + n).min(self.buffer.len())].to_vec(),
            ));
            self.pos += n;
        }
        n
    }
    fn push_to_index(&mut self, i: usize, allow_empty: bool) -> usize {
        let end = i.min(self.buffer.len()).max(self.pos);
        let n = end - self.pos;
        if n > 0 {
            self.push_count(n);
        } else if allow_empty {
            self.output.push(JsString::default());
        }
        n
    }
    fn push_indicators(&mut self) -> usize {
        let mut n = 0;
        loop {
            match self.char_at(0) {
                Some(33) => {
                    n += self.push_tag();
                    n += self.push_spaces(true);
                }
                Some(38) => {
                    n += self.push_until(not_anchor);
                    n += self.push_spaces(true);
                }
                Some(45 | 63 | 58) => {
                    let in_flow = self.flow_level > 0;
                    let ch1 = self.char_at(1);
                    if is_empty(ch1) || (in_flow && flow_indicator(ch1)) {
                        if !in_flow {
                            self.indent_next = self.indent_value + 1;
                        } else if self.flow_key {
                            self.flow_key = false;
                        }
                        n += self.push_count(1);
                        n += self.push_spaces(true);
                    } else {
                        break;
                    }
                }
                _ => break,
            }
        }
        n
    }
    fn push_tag(&mut self) -> usize {
        if self.char_at(1) == Some(60) {
            let mut i = self.pos + 2;
            while !is_empty(self.buffer.get(i).copied()) && self.buffer.get(i) != Some(&62) {
                i += 1;
            }
            self.push_to_index(
                if self.buffer.get(i) == Some(&62) {
                    i + 1
                } else {
                    i
                },
                false,
            )
        } else {
            let mut i = self.pos + 1;
            while let Some(ch) = self.buffer.get(i).copied() {
                if tag_char(ch) {
                    i += 1;
                } else if ch == 37
                    && hex(self.buffer.get(i + 1).copied())
                    && hex(self.buffer.get(i + 2).copied())
                {
                    i += 3;
                } else {
                    break;
                }
            }
            self.push_to_index(i, false)
        }
    }
    fn push_newline(&mut self) -> usize {
        match self.char_at(0) {
            Some(10) => self.push_count(1),
            Some(13) if self.char_at(1) == Some(10) => self.push_count(2),
            _ => 0,
        }
    }
    fn push_spaces(&mut self, allow_tabs: bool) -> usize {
        let mut i = self.pos;
        while self.buffer.get(i) == Some(&32) || (allow_tabs && self.buffer.get(i) == Some(&9)) {
            i += 1;
        }
        self.push_count(i - self.pos)
    }
    fn push_until(&mut self, test: impl Fn(Option<u16>) -> bool) -> usize {
        let mut i = self.pos;
        while !test(self.buffer.get(i).copied()) {
            i += 1;
        }
        self.push_to_index(i, false)
    }
}
