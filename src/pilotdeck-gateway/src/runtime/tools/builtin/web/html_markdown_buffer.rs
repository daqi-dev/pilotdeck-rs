//! Arena rope for Turndown's join/trim/line-prefix operations. Boundary
//! newlines are metadata; ancestor indentation is emitted only at flattening.
//! Both traversal and destruction are flat, including very wide documents.
use super::inline_code;
use crate::metadata::js_space;

/// Node v22 / V8's maximum string length, in UTF-16 code units.
pub const MAX_STRING_LENGTH: usize = crate::fetch_redirect::MAX_STRING_LENGTH;
pub const STRING_LIMIT_ERROR: &str = "Invalid string length";
pub(super) type Result<T> = std::result::Result<T, String>;

pub(super) fn check_length(length: usize) -> Result<()> {
    check_budget(length, MAX_STRING_LENGTH)
}
fn check_budget(length: usize, limit: usize) -> Result<()> {
    if length > limit {
        Err(STRING_LIMIT_ERROR.into())
    } else {
        Ok(())
    }
}
/// Validate the complete result before any allocation or append.
pub(super) fn concatenate(parts: &[&str]) -> Result<String> {
    check_length(parts.iter().map(|s| s.encode_utf16().count()).sum())?;
    Ok(parts.concat())
}

pub(super) fn repeat_string(text: &str, n: usize) -> Result<String> {
    check_length(Size::text(text).times(n).utf16)?;
    Ok(text.repeat(n))
}

#[derive(Clone, Copy)]
struct Size {
    bytes: usize,
    utf16: usize,
    newlines: usize,
    /// The whole content is ECMAScript whitespace (`String.prototype.trim`).
    space: bool,
    /// Trailing whitespace run.
    tail_units: usize,
    tail_bytes: usize,
    /// `\n` count in the trailing whitespace run.
    tail_newlines: usize,
    /// Code units/bytes after the last `\n` in the trailing run. When the run
    /// has no newline, this equals `tail_units`/`tail_bytes`.
    tail_after_newline_units: usize,
    tail_after_newline_bytes: usize,
}
impl Default for Size {
    fn default() -> Self {
        Self {
            bytes: 0,
            utf16: 0,
            newlines: 0,
            space: true,
            tail_units: 0,
            tail_bytes: 0,
            tail_newlines: 0,
            tail_after_newline_units: 0,
            tail_after_newline_bytes: 0,
        }
    }
}
impl Size {
    fn text(s: &str) -> Self {
        let tail_start = s.trim_end_matches(js_space).len();
        let suffix = &s[tail_start..];
        let tail_units = suffix.encode_utf16().count();
        let tail_newlines = suffix.bytes().filter(|&b| b == b'\n').count();
        let (after_units, after_bytes) = match suffix.rfind('\n') {
            Some(index) => {
                let after = &suffix[index + 1..];
                (after.encode_utf16().count(), after.len())
            }
            // With no newline the run is one contiguous space suffix.
            None => (tail_units, s.len() - tail_start),
        };
        Self {
            bytes: s.len(),
            utf16: s.encode_utf16().count(),
            newlines: s.bytes().filter(|&b| b == b'\n').count(),
            space: s.chars().all(js_space),
            tail_units,
            tail_bytes: s.len() - tail_start,
            tail_newlines,
            tail_after_newline_units: after_units,
            tail_after_newline_bytes: after_bytes,
        }
    }
    fn newlines(n: usize) -> Self {
        Self {
            bytes: n,
            utf16: n,
            newlines: n,
            space: true,
            tail_units: n,
            tail_bytes: n,
            tail_newlines: n,
            tail_after_newline_units: 0,
            tail_after_newline_bytes: 0,
        }
    }
    /// Concatenation, preserving the trailing whitespace run across the seam.
    fn concat(self, other: Self) -> Self {
        let (tail_units, tail_bytes, tail_newlines, after_units, after_bytes) = if other.space {
            let tail_units = other.utf16.saturating_add(self.tail_units);
            let tail_bytes = other.bytes.saturating_add(self.tail_bytes);
            let tail_newlines = other.tail_newlines.saturating_add(self.tail_newlines);
            let (after_units, after_bytes) = if other.tail_newlines > 0 {
                (
                    other.tail_after_newline_units,
                    other.tail_after_newline_bytes,
                )
            } else {
                (
                    other.utf16.saturating_add(self.tail_after_newline_units),
                    other.bytes.saturating_add(self.tail_after_newline_bytes),
                )
            };
            (
                tail_units,
                tail_bytes,
                tail_newlines,
                after_units,
                after_bytes,
            )
        } else {
            (
                other.tail_units,
                other.tail_bytes,
                other.tail_newlines,
                other.tail_after_newline_units,
                other.tail_after_newline_bytes,
            )
        };
        Self {
            bytes: self.bytes.saturating_add(other.bytes),
            utf16: self.utf16.saturating_add(other.utf16),
            newlines: self.newlines.saturating_add(other.newlines),
            space: self.space && other.space,
            tail_units,
            tail_bytes,
            tail_newlines,
            tail_after_newline_units: after_units,
            tail_after_newline_bytes: after_bytes,
        }
    }
    fn times(self, n: usize) -> Self {
        if n == 0 || self.utf16 == 0 {
            return Self::default();
        }
        let (tail_units, tail_bytes, tail_newlines, after_units, after_bytes) = if self.space {
            let tail_units = self.tail_units.saturating_mul(n);
            let tail_bytes = self.tail_bytes.saturating_mul(n);
            let tail_newlines = self.tail_newlines.saturating_mul(n);
            let (after_units, after_bytes) = if self.tail_newlines > 0 {
                (self.tail_after_newline_units, self.tail_after_newline_bytes)
            } else {
                (tail_units, tail_bytes)
            };
            (
                tail_units,
                tail_bytes,
                tail_newlines,
                after_units,
                after_bytes,
            )
        } else {
            (
                self.tail_units,
                self.tail_bytes,
                self.tail_newlines,
                self.tail_after_newline_units,
                self.tail_after_newline_bytes,
            )
        };
        Self {
            bytes: self.bytes.saturating_mul(n),
            utf16: self.utf16.saturating_mul(n),
            newlines: self.newlines.saturating_mul(n),
            space: self.space,
            tail_units,
            tail_bytes,
            tail_newlines,
            tail_after_newline_units: after_units,
            tail_after_newline_bytes: after_bytes,
        }
    }
    /// `String.replace(/\n/g, '\n' + prefix)`: Turndown's line-prefix indent.
    fn indent(self, prefix: Self) -> Self {
        debug_assert_eq!(prefix.newlines, 0);
        let bytes = self
            .bytes
            .saturating_add(prefix.bytes.saturating_mul(self.newlines));
        let utf16 = self
            .utf16
            .saturating_add(prefix.utf16.saturating_mul(self.newlines));
        let (tail_units, tail_bytes, tail_newlines, after_units, after_bytes) =
            if self.tail_units == 0 {
                (0, 0, 0, 0, 0)
            } else if self.tail_newlines == 0 {
                // The trailing run has no newline, so no prefix reaches the end.
                (
                    self.tail_units,
                    self.tail_bytes,
                    0,
                    self.tail_after_newline_units,
                    self.tail_after_newline_bytes,
                )
            } else if prefix.space {
                (
                    self.tail_units
                        .saturating_add(self.tail_newlines.saturating_mul(prefix.utf16)),
                    self.tail_bytes
                        .saturating_add(self.tail_newlines.saturating_mul(prefix.bytes)),
                    self.tail_newlines,
                    self.tail_after_newline_units.saturating_add(prefix.utf16),
                    self.tail_after_newline_bytes.saturating_add(prefix.bytes),
                )
            } else {
                // The prefix breaks the run; only its own spaces and the original
                // spaces after the last newline survive.
                let tail_units = prefix
                    .tail_units
                    .saturating_add(self.tail_after_newline_units);
                let tail_bytes = prefix
                    .tail_bytes
                    .saturating_add(self.tail_after_newline_bytes);
                (tail_units, tail_bytes, 0, tail_units, tail_bytes)
            };
        Self {
            bytes,
            utf16,
            newlines: self.newlines,
            // `String.replace(/\n/g, '\n' + prefix)` inserts nothing when the
            // content has no newline, so a whitespace-only content stays
            // whitespace even with a non-whitespace prefix (e.g. NBSP inside a
            // blockquote). Missing this made `concat` count the prefix's tail
            // space twice (real TS: `">"`, bounded path: `"> "`).
            space: self.space && (self.newlines == 0 || prefix.space),
            tail_units,
            tail_bytes,
            tail_newlines,
            tail_after_newline_units: after_units,
            tail_after_newline_bytes: after_bytes,
        }
    }
}

#[derive(Clone, Copy, Default)]
pub(super) struct Content {
    pub leading: usize,
    pub body: Option<usize>,
    pub trailing: usize,
}
impl Content {
    pub fn trim_newlines(self) -> Self {
        Self {
            leading: 0,
            trailing: 0,
            ..self
        }
    }
    pub fn prepend_newlines(mut self, n: usize) -> Self {
        self.leading += n;
        self
    }
    pub fn append_newlines(mut self, n: usize) -> Self {
        if self.body.is_some() {
            self.trailing += n;
        } else {
            self.leading += n;
        }
        self
    }
    pub fn wrap_block(self) -> Self {
        self.prepend_newlines(2).append_newlines(2)
    }
    fn empty(self) -> bool {
        self.body.is_none() && self.leading == 0
    }
}
enum Part {
    Released,
    Text(String),
    /// A prefix of the node's logical text. `Node::size` still describes the
    /// full value, so length-only consumers (final trim, heading underline,
    /// cache accounting) never materialize the tail. Only the bounded path
    /// creates these; a full conversion never does.
    TextPartial(String, Box<CodeShape>),
    Repeat(String, usize),
    Pair(Content, Content),
    Indent(usize, String),
}
struct Node {
    part: Part,
    blank: bool,
    size: Size,
}
pub(super) struct Rope {
    limit: usize,
    /// Set by `convert_bounded`: how many UTF-16 units of the final trimmed
    /// output the caller may need. `usize::MAX` for an unbounded conversion.
    prefix_units: Option<usize>,
    /// True once a `TextPartial` exists; a full `flatten` would be a lie then.
    has_partial: bool,
    nodes: Vec<Node>,
    #[cfg(test)]
    text_bytes: usize,
    #[cfg(test)]
    peak_text_bytes: usize,
    #[cfg(test)]
    peak_flatten_bytes: std::cell::Cell<usize>,
    /// Most characters one `flatten_span` call visited. A bounded prefix must
    /// stop after roughly `skip + emit` characters; visiting the whole logical
    /// output is the amplification regression Grok r3 P1 reported.
    #[cfg(test)]
    peak_span_visits: std::cell::Cell<usize>,
}
impl Default for Rope {
    fn default() -> Self {
        Self {
            limit: MAX_STRING_LENGTH,
            prefix_units: None,
            has_partial: false,
            nodes: Vec::new(),
            #[cfg(test)]
            text_bytes: 0,
            #[cfg(test)]
            peak_text_bytes: 0,
            #[cfg(test)]
            peak_flatten_bytes: std::cell::Cell::new(0),
            #[cfg(test)]
            peak_span_visits: std::cell::Cell::new(0),
        }
    }
}
fn is_leading_char(ch: char) -> bool {
    matches!(ch, '\t' | '\r' | '\n')
}

/// Consume Turndown's final `^[\t\r\n]+` prefix from `n` newline units, each
/// followed by the active indent prefixes.
fn leading_newlines(n: usize, prefixes: &[&str], leading: &mut usize, done: &mut bool) {
    if *done || n == 0 {
        return;
    }
    let mut unit_units = 1;
    *leading += 1;
    for prefix in prefixes {
        for ch in prefix.chars() {
            unit_units += ch.len_utf16();
            if is_leading_char(ch) {
                *leading += 1;
            } else {
                *done = true;
                return;
            }
        }
    }
    if n > 1 {
        *leading += (n - 1).saturating_mul(unit_units);
    }
}

/// Same for a text/repeat chunk with `copies` repetitions of `text`.
fn leading_text(
    text: &str,
    copies: usize,
    prefixes: &[&str],
    leading: &mut usize,
    done: &mut bool,
) {
    if *done || copies == 0 || text.is_empty() {
        return;
    }
    if copies > 1 && prefixes.is_empty() && text.chars().all(is_leading_char) {
        *leading += copies.saturating_mul(text.encode_utf16().count());
        return;
    }
    for _ in 0..copies {
        for ch in text.chars() {
            if !is_leading_char(ch) {
                *done = true;
                return;
            }
            *leading += 1;
            if ch == '\n' {
                for prefix in prefixes {
                    for ch in prefix.chars() {
                        if !is_leading_char(ch) {
                            *done = true;
                            return;
                        }
                        *leading += 1;
                    }
                }
            }
        }
    }
}

enum Chunk<'a> {
    Text(&'a str),
    /// The materialized prefix of a `TextPartial`; `Shape` carries the rest.
    PartialText(&'a str),
    Repeat(&'a str, usize),
    /// Whole logical text of a `TextPartial`, including the unmaterialized tail.
    Shape(&'a CodeShape),
}

fn code_char(ch: char) -> char {
    if ch == '\n' || ch == '\r' { ' ' } else { ch }
}

/// UTF-16 units of `text` after Turndown's inline-code folding: each
/// `/\r?\n|\r/g` match becomes one space, so an adjacent CR LF pair counts
/// once.
fn folded_len(text: &str) -> usize {
    let mut units = 0usize;
    let mut pending_cr = false;
    for ch in text.chars() {
        if pending_cr {
            pending_cr = false;
            if ch == '\n' {
                continue;
            }
        }
        if ch == '\r' {
            pending_cr = true;
        }
        units = units.saturating_add(ch.len_utf16());
    }
    units
}

/// The parts of Turndown's inline-code rule that depend on the whole content:
/// backtick run lengths (delimiter choice), first/last char, "any non-space"
/// (padding), plus CR/LF normalization. `TextPartial` stores this so a later
/// `<code>` sees backticks in the tail that were never materialized.
#[derive(Clone, Default)]
struct CodeShape {
    runs: std::collections::HashSet<usize>,
    /// Current, not yet completed trailing backtick run.
    run: usize,
    /// Leading backtick run; stays 0 when the text starts with another char.
    lead_run: usize,
    seen_non_backtick: bool,
    first: Option<char>,
    last: Option<char>,
    any_non_space: bool,
    has_newline: bool,
    /// Number of adjacent CR LF pairs in the raw text. Turndown's inline-code
    /// rule folds newlines with `content.replace(/\r?\n|\r/g, " ")`, so a pair
    /// is one output unit (and one byte) where `utf16` counted two.
    crlf_pairs: usize,
    /// Raw content units: the units `size()` reports for this span.
    utf16: usize,
}
impl CodeShape {
    fn is_empty(&self) -> bool {
        self.first.is_none()
    }
    fn finish_run(&mut self) {
        if self.run > 0 {
            self.runs.insert(self.run);
            self.run = 0;
        }
    }
    fn push(&mut self, ch: char) {
        if ch == '\n' {
            self.has_newline = true;
        }
        let mapped = code_char(ch);
        self.utf16 = self.utf16.saturating_add(ch.len_utf16());
        if self.first.is_none() {
            self.first = Some(mapped);
        }
        self.last = Some(mapped);
        if mapped != ' ' {
            self.any_non_space = true;
        }
        if ch == '`' {
            self.run = self.run.saturating_add(1);
            if !self.seen_non_backtick {
                self.lead_run = self.run;
            }
        } else {
            if !self.seen_non_backtick {
                self.lead_run = self.run;
                self.seen_non_backtick = true;
            }
            self.finish_run();
        }
    }
    /// Append a complete shape that follows this text.
    fn merge(&mut self, other: &CodeShape) {
        if other.is_empty() {
            return;
        }
        if self.is_empty() {
            *self = other.clone();
            return;
        }
        let boundary = self.run.saturating_add(other.lead_run);
        if boundary > 0 {
            self.runs.insert(boundary);
        }
        self.runs.extend(other.runs.iter().copied());
        self.first = self.first.or(other.first);
        self.last = other.last.or(self.last);
        self.any_non_space |= other.any_non_space;
        self.has_newline |= other.has_newline;
        self.crlf_pairs = self.crlf_pairs.saturating_add(other.crlf_pairs);
        self.utf16 = self.utf16.saturating_add(other.utf16);
        if other.seen_non_backtick {
            self.seen_non_backtick = true;
            self.run = other.run;
        } else {
            self.run = self.run.saturating_add(other.run);
            if !self.seen_non_backtick {
                self.lead_run = self.run;
            }
        }
    }
    fn delimiter(&self) -> usize {
        let mut count = 1;
        while self.runs.contains(&count) {
            count += 1;
        }
        count
    }
    fn padded(&self) -> bool {
        self.first == Some('`')
            || self.last == Some('`')
            || (self.first == Some(' ') && self.last == Some(' ') && self.any_non_space)
    }
}

impl Rope {
    fn size(&self, content: Content) -> Size {
        let body = content
            .body
            .map_or(Size::default(), |id| self.nodes[id].size);
        Size::newlines(content.leading)
            .concat(body)
            .concat(Size::newlines(content.trailing))
    }
    pub fn check(&self, content: Content) -> Result<Content> {
        check_budget(self.size(content).utf16, self.limit)?;
        Ok(content)
    }
    pub fn utf16_len(&self, content: Content) -> usize {
        self.size(content).utf16
    }
    fn push(&mut self, part: Part, blank: bool, size: Size) -> Result<usize> {
        check_budget(size.utf16, self.limit)?;
        if matches!(part, Part::TextPartial(..)) {
            self.has_partial = true;
        }
        #[cfg(test)]
        {
            self.text_bytes += Self::part_bytes(&part);
            self.peak_text_bytes = self.peak_text_bytes.max(self.text_bytes);
        }
        let id = self.nodes.len();
        self.nodes.push(Node { part, blank, size });
        Ok(id)
    }
    #[cfg(test)]
    fn part_bytes(part: &Part) -> usize {
        match part {
            Part::Text(text)
            | Part::TextPartial(text, _)
            | Part::Indent(_, text)
            | Part::Repeat(text, _) => text.capacity(),
            _ => 0,
        }
    }
    /// The caller of a bounded conversion tells the renderer how much of the
    /// final output can still matter, so replacements materialize only a
    /// prefix instead of the whole subtree.
    pub fn set_prefix_units(&mut self, units: usize) {
        self.prefix_units = Some(units);
    }
    fn prefix_need(&self) -> usize {
        // One unit covers a flank trim eating the inline-code pad; another
        // covers a surrogate pair at the cut.
        self.prefix_units
            .map_or(usize::MAX, |units| units.saturating_add(4))
    }
    #[cfg(test)]
    pub fn peak_text_bytes(&self) -> usize {
        self.peak_text_bytes
    }
    #[cfg(test)]
    pub fn peak_flatten_bytes(&self) -> usize {
        self.peak_flatten_bytes.get()
    }
    #[cfg(test)]
    pub fn peak_span_visits(&self) -> usize {
        self.peak_span_visits.get()
    }
    #[cfg(test)]
    pub fn with_limit(limit: usize) -> Self {
        Self {
            limit,
            ..Self::default()
        }
    }
    /// Drop the strings of a subtree the renderer just replaced. The renderer
    /// reads every child once; IDs stay stable and releasing a node twice is
    /// harmless.
    fn release(&mut self, content: Content) {
        let mut work: Vec<_> = content.body.into_iter().collect();
        while let Some(id) = work.pop() {
            let part = std::mem::replace(&mut self.nodes[id].part, Part::Released);
            #[cfg(test)]
            {
                self.text_bytes -= Self::part_bytes(&part);
            }
            match part {
                Part::Pair(left, right) => {
                    work.extend(left.body);
                    work.extend(right.body);
                }
                Part::Indent(child, _) => work.push(child),
                Part::Text(_) | Part::TextPartial(..) | Part::Repeat(_, _) | Part::Released => {}
            }
        }
    }
    /// Whether this subtree holds a bounded prefix. A global flag would make
    /// an unrelated small replacement take the partial path (or trip the
    /// full-flatten assertion), so check the content being flattened.
    fn contains_partial(&self, content: Content) -> bool {
        let mut work: Vec<usize> = content.body.into_iter().collect();
        while let Some(id) = work.pop() {
            match &self.nodes[id].part {
                Part::TextPartial(..) => return true,
                Part::Pair(left, right) => {
                    work.extend(left.body);
                    work.extend(right.body);
                }
                Part::Indent(child, _) => work.push(*child),
                Part::Text(_) | Part::Repeat(_, _) | Part::Released => {}
            }
        }
        false
    }
    /// Keep a materialized prefix while claiming the full logical size.
    fn text_partial(&mut self, text: String, size: Size, shape: CodeShape) -> Result<Content> {
        check_budget(size.utf16, self.limit)?;
        Ok(Content {
            body: Some(self.push(Part::TextPartial(text, Box::new(shape)), false, size)?),
            ..Content::default()
        })
    }
    pub fn text(&mut self, text: &str) -> Result<Content> {
        check_budget(text.encode_utf16().count(), self.limit)?;
        let leading = text.len() - text.trim_start_matches('\n').len();
        let rest = &text[leading..];
        let body = rest.trim_end_matches('\n');
        let body_id = if body.is_empty() {
            None
        } else {
            Some(self.push(
                Part::Text(body.into()),
                body.chars().all(js_space),
                Size::text(body),
            )?)
        };
        Ok(Content {
            leading,
            body: body_id,
            trailing: rest.len() - body.len(),
        })
    }
    /// Keep an already materialized replacement's allocation for reuse.
    pub fn text_owned(&mut self, mut text: String) -> Result<Content> {
        check_budget(text.encode_utf16().count(), self.limit)?;
        let leading = text.len() - text.trim_start_matches('\n').len();
        let end = text.trim_end_matches('\n').len().max(leading);
        let trailing = text.len() - end;
        text.truncate(end);
        text.drain(..leading);
        let body = if text.is_empty() {
            None
        } else {
            let blank = text.chars().all(js_space);
            let size = Size::text(&text);
            Some(self.push(Part::Text(text), blank, size)?)
        };
        Ok(Content {
            leading,
            trailing,
            body,
        })
    }
    /// Repeated markers stay symbolic until a validated final flatten. In
    /// particular, rejected headings never allocate hundreds of MiB first.
    pub fn repeat(&mut self, text: &str, n: usize) -> Result<Content> {
        let size = Size::text(text).times(n);
        check_budget(size.utf16, self.limit)?;
        if n == 0 || text.is_empty() {
            return Ok(Content::default());
        }
        debug_assert!(!text.contains('\n'));
        Ok(Content {
            body: Some(self.push(
                Part::Repeat(text.into(), n),
                text.chars().all(js_space),
                size,
            )?),
            ..Content::default()
        })
    }
    pub fn blank(&self, content: Content) -> bool {
        content.body.is_none_or(|id| self.nodes[id].blank)
    }
    /// Concatenation without Turndown's boundary newline normalization.
    pub fn concat(&mut self, mut left: Content, mut right: Content) -> Result<Content> {
        check_budget(self.size(left).concat(self.size(right)).utf16, self.limit)?;
        if left.body.is_none() {
            right.leading += left.leading;
            return Ok(right);
        }
        if right.body.is_none() {
            left.trailing += right.leading;
            return Ok(left);
        }
        let leading = left.leading;
        let trailing = right.trailing;
        left.leading = 0;
        right.trailing = 0;
        let blank = self.blank(left) && self.blank(right);
        let size = self.size(left).concat(self.size(right));
        Ok(Content {
            leading,
            trailing,
            body: Some(self.push(Part::Pair(left, right), blank, size)?),
        })
    }
    pub fn join(&mut self, mut left: Content, mut right: Content) -> Result<Content> {
        if left.empty() {
            right.leading = right.leading.min(2);
            return self.check(right);
        }
        let trailing = if left.body.is_none() {
            left.leading
        } else {
            left.trailing
        };
        let boundary = trailing.max(right.leading).min(2);
        if left.body.is_none() {
            left.leading = boundary;
        } else {
            left.trailing = boundary;
        }
        right.leading = 0;
        self.concat(left, right)
    }
    pub fn indent(&mut self, content: Content, prefix: &str) -> Result<Content> {
        let size = self.size(content);
        let prefix_size = Size::text(prefix);
        check_budget(size.indent(prefix_size).utf16, self.limit)?;
        // Isolate boundary newlines: their inserted prefixes become body text.
        let mut result = Content::default();
        if content.leading > 0 {
            let text = repeat_string(&concatenate(&["\n", prefix])?, content.leading)?;
            result = self.text(&text)?;
        }
        if let Some(id) = content.body {
            let blank = self.nodes[id].blank && prefix.chars().all(js_space);
            let body = Content {
                body: Some(self.push(
                    Part::Indent(id, prefix.into()),
                    blank,
                    self.nodes[id].size.indent(prefix_size),
                )?),
                ..Content::default()
            };
            result = self.concat(result, body)?;
        }
        if content.trailing > 0 {
            let text = self.text(&repeat_string(
                &concatenate(&["\n", prefix])?,
                content.trailing,
            )?)?;
            result = self.concat(result, text)?;
        }
        Ok(result)
    }
    /// Length of Turndown's final `^[\t\r\n]+` trim, plus a `Size` whose
    /// trailing fields describe the final `[\t\r\n\s]+$` trim. Neither pass
    /// materializes the output: text nodes are scanned once (the input is
    /// capped at 10 MiB), and repeated prefixes/newlines use O(1) arithmetic.
    fn measure(&self, content: Content) -> Result<(usize, Size)> {
        self.check(content)?;
        enum Task {
            Content(Content),
            Body(usize),
            Newlines(usize),
            PopPrefix,
        }
        let mut work = vec![Task::Content(content)];
        let mut prefixes: Vec<&str> = Vec::new();
        let mut prefix_sizes: Vec<Size> = Vec::new();
        let mut prefix_size = Size::default();
        let mut trailing = Size::default();
        let mut leading = 0usize;
        let mut leading_done = false;
        while let Some(task) = work.pop() {
            match task {
                Task::Content(content) => {
                    work.push(Task::Newlines(content.trailing));
                    if let Some(id) = content.body {
                        work.push(Task::Body(id));
                    }
                    work.push(Task::Newlines(content.leading));
                }
                Task::Newlines(n) => {
                    if n == 0 {
                        continue;
                    }
                    let unit = Size::newlines(1).concat(prefix_size);
                    trailing = trailing.concat(unit.times(n));
                    leading_newlines(n, &prefixes, &mut leading, &mut leading_done);
                }
                Task::PopPrefix => {
                    prefixes.pop();
                    prefix_size = prefix_sizes.pop().expect("prefix size");
                }
                Task::Body(id) => {
                    let node = &self.nodes[id];
                    match &node.part {
                        Part::Released => unreachable!("consumed rope subtree"),
                        // `node.size` is the logical size, including the tail of
                        // a `TextPartial` that was never materialized.
                        Part::Text(text) | Part::TextPartial(text, _) => {
                            trailing = trailing.concat(node.size.indent(prefix_size));
                            leading_text(text, 1, &prefixes, &mut leading, &mut leading_done);
                        }
                        Part::Repeat(text, n) => {
                            trailing = trailing.concat(node.size.indent(prefix_size));
                            leading_text(text, *n, &prefixes, &mut leading, &mut leading_done);
                        }
                        Part::Pair(left, right) => {
                            work.push(Task::Content(*right));
                            work.push(Task::Content(*left));
                        }
                        Part::Indent(id, prefix) => {
                            prefixes.push(prefix);
                            prefix_sizes.push(prefix_size);
                            prefix_size = prefix_size.concat(Size::text(prefix));
                            work.push(Task::PopPrefix);
                            work.push(Task::Body(*id));
                        }
                    }
                }
            }
        }
        Ok((leading, trailing))
    }

    /// Visit the flattened output in order without allocating. `Text` is a
    /// literal chunk; `Repeat` is a chunk repeated `n` times. Indent prefixes
    /// are inserted after every newline, exactly like `flatten`.
    fn walk<'a, F>(&'a self, content: Content, mut visit: F) -> bool
    where
        F: FnMut(Chunk<'a>) -> bool,
    {
        enum Task {
            Content(Content),
            Body(usize),
            Newlines(usize),
            PopPrefix,
            Text(usize, usize),
            Shape(usize),
        }
        let mut work = vec![Task::Content(content)];
        let mut prefixes: Vec<&'a str> = Vec::new();
        while let Some(task) = work.pop() {
            match task {
                Task::Content(content) => {
                    work.push(Task::Newlines(content.trailing));
                    if let Some(id) = content.body {
                        work.push(Task::Body(id));
                    }
                    work.push(Task::Newlines(content.leading));
                }
                Task::Newlines(n) => {
                    for _ in 0..n {
                        if !visit(Chunk::Text("\n")) {
                            return false;
                        }
                        for prefix in &prefixes {
                            if !visit(Chunk::Text(prefix)) {
                                return false;
                            }
                        }
                    }
                }
                Task::Shape(id) => {
                    let Part::TextPartial(_, shape) = &self.nodes[id].part else {
                        unreachable!("shape task on a non-partial node");
                    };
                    if !visit(Chunk::Shape(shape)) {
                        return false;
                    }
                }
                Task::PopPrefix => {
                    prefixes.pop();
                }
                Task::Text(id, offset) => {
                    let (text, partial) = match &self.nodes[id].part {
                        Part::Text(text) => (text, false),
                        Part::TextPartial(text, _) => (text, true),
                        _ => unreachable!("text task on a non-text node"),
                    };
                    let end = text[offset..]
                        .find('\n')
                        .map_or(text.len(), |index| offset + index + 1);
                    let raw = &text[offset..end];
                    let chunk = if partial {
                        Chunk::PartialText(raw)
                    } else {
                        Chunk::Text(raw)
                    };
                    if !raw.is_empty() && !visit(chunk) {
                        return false;
                    }
                    if raw.ends_with('\n') {
                        for prefix in &prefixes {
                            if !visit(Chunk::Text(prefix)) {
                                return false;
                            }
                        }
                    }
                    if end < text.len() {
                        work.push(Task::Text(id, end));
                    }
                }
                Task::Body(id) => match &self.nodes[id].part {
                    Part::Released => unreachable!("consumed rope subtree"),
                    Part::Text(_) => work.push(Task::Text(id, 0)),
                    Part::TextPartial(..) => {
                        work.push(Task::Text(id, 0));
                        work.push(Task::Shape(id));
                    }
                    Part::Repeat(text, n) => {
                        if !visit(Chunk::Repeat(text, *n)) {
                            return false;
                        }
                    }
                    Part::Pair(left, right) => {
                        work.push(Task::Content(*right));
                        work.push(Task::Content(*left));
                    }
                    Part::Indent(id, prefix) => {
                        prefixes.push(prefix);
                        work.push(Task::PopPrefix);
                        work.push(Task::Body(*id));
                    }
                },
            }
        }
        true
    }

    /// Flatten the unit range `[skip, skip + emit)` of `content` (one whole
    /// code point may overshoot by one unit). Allocation is bounded by the
    /// range, not by the symbolic output.
    ///
    /// With `replace_newlines`, `skip`/`emit` count units of the *folded*
    /// output: Turndown's inline-code rule is
    /// `content.replace(/\r?\n|\r/g, " ")`, so CR LF is one match (one space)
    /// and a pair is matched across node boundaries here.
    pub fn flatten_span(
        &self,
        content: Content,
        skip_units: usize,
        emit_units: usize,
        replace_newlines: bool,
    ) -> Result<String> {
        self.check(content)?;
        let total = self.size(content);
        let skip = skip_units.min(total.utf16);
        let emit = emit_units.min(total.utf16 - skip);
        // `flatten` copies newlines and CRs verbatim; the bulk path is only
        // equivalent when the caller does not ask for Turndown's inline-code
        // whitespace folding.
        if skip == 0 && emit >= total.utf16 && !self.contains_partial(content) && !replace_newlines
        {
            return self.flatten(content);
        }
        let cap = emit.saturating_mul(4).min(total.bytes);
        #[cfg(test)]
        self.peak_flatten_bytes
            .set(self.peak_flatten_bytes.get().max(cap));
        let mut out = String::with_capacity(cap);
        let mut skipped = 0usize;
        let mut written = 0usize;
        // A CR already emitted as one space; a following LF belongs to the same
        // regex match and must not become a second space.
        let mut pending_cr = false;
        #[cfg(test)]
        let mut visited = 0usize;
        let completed = self.walk(content, |chunk| {
            let (text, copies, partial) = match chunk {
                Chunk::Text(text) => (text, 1usize, false),
                Chunk::PartialText(text) => (text, 1usize, true),
                Chunk::Repeat(text, copies) => (text, copies, false),
                Chunk::Shape(_) => return true,
            };
            for _ in 0..copies {
                for ch in text.chars() {
                    let mut ch = ch;
                    if replace_newlines {
                        if pending_cr {
                            pending_cr = false;
                            if ch == '\n' {
                                #[cfg(test)]
                                {
                                    visited += 1;
                                }
                                continue;
                            }
                        }
                        if ch == '\r' {
                            pending_cr = true;
                            ch = ' ';
                        } else if ch == '\n' {
                            ch = ' ';
                        }
                    }
                    #[cfg(test)]
                    {
                        visited += 1;
                    }
                    let units = ch.len_utf16();
                    if skipped < skip {
                        skipped = skipped.saturating_add(units);
                        continue;
                    }
                    if written >= emit {
                        return false;
                    }
                    out.push(ch);
                    written += units;
                }
            }
            if partial {
                // A `TextPartial` materializes only a prefix; its tail sits
                // between that prefix and the next chunk, so a trailing CR
                // must not pair with the next chunk's leading LF. This is the
                // same seam where `analyze_inline_code_span` clears
                // `last_pushed` on the partial's `Chunk::Shape`.
                pending_cr = false;
            }
            true
        });
        debug_assert_eq!(skipped, skip, "unit skip must land on a char boundary");
        // A folded CR LF pair consumes two input chars for one output unit, so
        // the bound is twice `emit`. `completed` means the content ran out
        // before reaching `emit`, which is only legal for an exact span.
        debug_assert!(written <= emit.saturating_add(1), "span over-emitted");
        debug_assert!(
            completed || written >= emit,
            "partial prefix wrote {written} units for emit={emit}"
        );
        #[cfg(test)]
        {
            self.peak_span_visits
                .set(self.peak_span_visits.get().max(visited));
            debug_assert!(
                visited
                    <= skip
                        .saturating_add(emit.saturating_mul(2))
                        .saturating_add(2),
                "flatten_span visited {visited} chars for skip={skip} emit={emit}"
            );
        }
        Ok(out)
    }

    /// Shape of the whole content (or of the unit range a trim keeps).
    /// `TextPartial` contributes its stored full shape, not its prefix.
    fn analyze_inline_code_span(
        &self,
        content: Content,
        skip_units: usize,
        emit_units: usize,
    ) -> CodeShape {
        let total = self.size(content).utf16;
        let skip = skip_units.min(total);
        let emit = emit_units.min(total - skip);
        let mut shape = CodeShape::default();
        let mut skipped = 0usize;
        let mut used = 0usize;
        // Last character actually pushed. A pushed LF right after a pushed CR
        // is one folded unit for Turndown's `\r?\n|\r` replacement, not two.
        let mut last_pushed: Option<char> = None;
        let add_char = |shape: &mut CodeShape,
                        ch: char,
                        skipped: &mut usize,
                        used: &mut usize,
                        last: &mut Option<char>| {
            let units = ch.len_utf16();
            if *skipped < skip {
                *skipped = skipped.saturating_add(units);
                return true;
            }
            if *used >= emit {
                return false;
            }
            if ch == '\n' && *last == Some('\r') {
                shape.crlf_pairs = shape.crlf_pairs.saturating_add(1);
            }
            shape.push(ch);
            *last = Some(ch);
            *used = used.saturating_add(units);
            true
        };
        self.walk(content, |chunk| match chunk {
            Chunk::Shape(other) => {
                if skipped < skip && skipped.saturating_add(other.utf16) <= skip {
                    skipped = skipped.saturating_add(other.utf16);
                    return true;
                }
                if used >= emit {
                    return false;
                }
                shape.merge(other);
                skipped = skipped.saturating_add(other.utf16);
                used = used.saturating_add(other.utf16);
                // An inline-code span starts and ends with a non-space
                // (delimiter or trimmed text), so it cannot continue a CR LF
                // pair across the seam.
                last_pushed = None;
                true
            }
            // A partial's materialized prefix is already inside `Shape`.
            Chunk::PartialText(_) => true,
            Chunk::Text(text) => {
                for ch in text.chars() {
                    if !add_char(&mut shape, ch, &mut skipped, &mut used, &mut last_pushed) {
                        return false;
                    }
                }
                true
            }
            Chunk::Repeat(text, copies) => {
                for _ in 0..copies {
                    for ch in text.chars() {
                        if !add_char(&mut shape, ch, &mut skipped, &mut used, &mut last_pushed) {
                            return false;
                        }
                    }
                }
                true
            }
        });
        shape
    }

    fn analyze_inline_code(&self, content: Content) -> CodeShape {
        self.analyze_inline_code_span(content, 0, usize::MAX)
    }

    /// Units/bytes/newlines of the leading JS-whitespace run.
    fn leading_space(&self, content: Content) -> (usize, usize, usize) {
        let mut units = 0usize;
        let mut bytes = 0usize;
        let mut newlines = 0usize;
        self.walk(content, |chunk| {
            let (text, copies) = match chunk {
                Chunk::Text(text) | Chunk::PartialText(text) => (text, 1usize),
                Chunk::Repeat(text, copies) => (text, copies),
                Chunk::Shape(_) => return true,
            };
            if text.chars().all(js_space) {
                let size = Size::text(text);
                units += size.utf16.saturating_mul(copies);
                bytes += size.bytes.saturating_mul(copies);
                newlines += size.newlines.saturating_mul(copies);
                return true;
            }
            for ch in text.chars() {
                if !js_space(ch) {
                    return false;
                }
                units += ch.len_utf16();
                bytes += ch.len_utf8();
                if ch == '\n' {
                    newlines += 1;
                }
            }
            true
        });
        (units, bytes, newlines)
    }

    /// `content.trim()` before Turndown applies an inline node's flanks.
    pub fn trim_content(&mut self, content: Content) -> Result<Content> {
        let size = self.size(content);
        if size.space {
            self.release(content);
            return Ok(Content::default());
        }
        let (lead_units, lead_bytes, lead_newlines) = self.leading_space(content);
        let trimmed_units = size
            .utf16
            .saturating_sub(lead_units)
            .saturating_sub(size.tail_units);
        let trimmed_bytes = size
            .bytes
            .saturating_sub(lead_bytes)
            .saturating_sub(size.tail_bytes);
        let trimmed_newlines = size
            .newlines
            .saturating_sub(lead_newlines)
            .saturating_sub(size.tail_newlines);
        if trimmed_units == 0 {
            self.release(content);
            return Ok(Content::default());
        }
        let need = self.prefix_need();
        let partial = trimmed_units > need;
        let shape =
            partial.then(|| self.analyze_inline_code_span(content, lead_units, trimmed_units));
        let mut emit = trimmed_units.min(need);
        let mut text = self.flatten_span(content, lead_units, emit, false)?;
        if partial {
            // Turndown's inline-code rule folds every CR LF pair into one
            // unit (`/\r?\n|\r/g`), so a `need`-unit raw prefix can cover
            // fewer folded units. An enclosing `<code>` folds this prefix and
            // would then walk past its materialized end, skipping the
            // unmaterialized tail; grow the prefix until its folded length
            // reaches `need`, or the whole trimmed content is kept.
            loop {
                let folded = folded_len(&text);
                if folded >= need || emit >= trimmed_units {
                    break;
                }
                emit = emit.saturating_add(need - folded).min(trimmed_units);
                text = self.flatten_span(content, lead_units, emit, false)?;
            }
        }
        self.release(content);
        if let Some(shape) = shape {
            self.text_partial(
                text,
                Size {
                    bytes: trimmed_bytes,
                    utf16: trimmed_units,
                    newlines: trimmed_newlines,
                    space: false,
                    ..Size::default()
                },
                shape,
            )
        } else {
            self.text_owned(text)
        }
    }

    /// Turndown's default inline-code rule. Only the prefix the final
    /// truncation can need is materialized; delimiter choice and padding are
    /// computed from the symbolic stream.
    pub fn inline_code(&mut self, content: Content) -> Result<Content> {
        let size = self.size(content);
        if size.utf16 == 0 {
            self.release(content);
            return Ok(Content::default());
        }
        let mut shape = self.analyze_inline_code(content);
        shape.finish_run();
        let count = shape.delimiter();
        let pad = usize::from(shape.padded());
        let wrapper = count.saturating_add(pad);
        // Turndown folds `\r\n` to one space *before* measuring the span, so a
        // pair contributes one unit and one byte where `size` counted two.
        let folded_units = shape.utf16.saturating_sub(shape.crlf_pairs);
        let folded_bytes = size.bytes.saturating_sub(shape.crlf_pairs);
        let out = Size {
            bytes: folded_bytes.saturating_add(wrapper.saturating_mul(2)),
            utf16: folded_units.saturating_add(wrapper.saturating_mul(2)),
            newlines: 0,
            space: false,
            ..Size::default()
        };
        check_budget(out.utf16, self.limit)?;
        let need = self.prefix_need();
        if out.utf16 <= need && !self.contains_partial(content) {
            // The whole span fits in the truncation prefix (including CR/LF
            // content: the bulk path and `inline_code` fold it exactly like
            // Turndown). Anything larger goes to the bounded assembly below,
            // which folds CR LF pairs symbolically instead of flattening the
            // whole subtree.
            let inner = self.flatten(content)?;
            self.release(content);
            return self.text_owned(inline_code(inner)?);
        }
        let inner_need = need.saturating_sub(wrapper);
        // When the inner content fits, also materialize the closing delimiter
        // and pad. Without them the bounded prefix can end `wrapper` units
        // short of the logical code span even though the final take needs it.
        let full_inner = folded_units <= inner_need;
        let inner_emit = inner_need.min(folded_units);
        let closing = if full_inner { wrapper } else { 0 };
        let mut text = String::with_capacity(
            wrapper
                .saturating_add(inner_emit)
                .saturating_add(closing)
                .saturating_mul(4),
        );
        text.extend(std::iter::repeat_n('`', count));
        if pad == 1 {
            text.push(' ');
        }
        let inner = self.flatten_span(content, 0, inner_emit, true)?;
        text.push_str(&inner);
        if full_inner {
            if pad == 1 {
                text.push(' ');
            }
            text.extend(std::iter::repeat_n('`', count));
        }
        self.release(content);
        // Full logical shape of the emitted code span, so an outer `<code>`
        // still sees backticks beyond this prefix. The emitted span contains no
        // CR/LF any more: folding already happened.
        let mut out_shape = CodeShape::default();
        for _ in 0..count {
            out_shape.push('`');
        }
        if pad == 1 {
            out_shape.push(' ');
        }
        let mut inner_shape = shape;
        inner_shape.utf16 = folded_units;
        inner_shape.crlf_pairs = 0;
        inner_shape.has_newline = false;
        out_shape.merge(&inner_shape);
        if pad == 1 {
            out_shape.push(' ');
        }
        for _ in 0..count {
            out_shape.push('`');
        }
        self.text_partial(text, out, out_shape)
    }

    /// The full trimmed Markdown when it fits `prefix_units`, otherwise a
    /// prefix of at least `prefix_units` units of it, plus the full trimmed
    /// UTF-16/UTF-8 lengths. This is `truncateMarkdown(turndown(html))`
    /// without materializing the part after the truncation point.
    pub fn flatten_bounded(
        &self,
        content: Content,
        prefix_units: usize,
    ) -> Result<(String, usize, usize, bool)> {
        self.check(content)?;
        let total = self.size(content);
        if total.space {
            return Ok((String::new(), 0, 0, true));
        }
        let (leading, trailing) = self.measure(content)?;
        let trimmed_units = total
            .utf16
            .saturating_sub(leading)
            .saturating_sub(trailing.tail_units);
        let trimmed_bytes = total
            .bytes
            .saturating_sub(leading)
            .saturating_sub(trailing.tail_bytes);
        let complete = trimmed_units <= prefix_units;
        let take = trimmed_units.min(prefix_units);
        let text = if !self.has_partial && leading.saturating_add(take) >= total.utf16 {
            // The whole trailing side is needed anyway; reuse `flatten`'s
            // bulk `push_str` fast path. `leading` is `[\t\r\n]`, one byte.
            let mut text = self.flatten(content)?;
            debug_assert!(text.is_char_boundary(leading));
            text.drain(..leading.min(text.len()));
            text
        } else {
            self.flatten_span(content, leading, take, false)?
        };
        Ok((text, trimmed_units, trimmed_bytes, complete))
    }

    pub fn flatten(&self, content: Content) -> Result<String> {
        debug_assert!(
            !self.contains_partial(content),
            "full flatten of a bounded prefix would silently drop its tail"
        );
        self.check(content)?;
        enum Task {
            Content(Content),
            Body(usize),
            Newlines(usize),
            PopPrefix,
        }
        let mut work = vec![Task::Content(content)];
        let mut prefixes: Vec<&str> = Vec::new();
        let capacity = self.size(content).bytes;
        #[cfg(test)]
        self.peak_flatten_bytes
            .set(self.peak_flatten_bytes.get().max(capacity));
        let mut out = String::with_capacity(capacity);
        while let Some(task) = work.pop() {
            match task {
                Task::Content(content) => {
                    work.push(Task::Newlines(content.trailing));
                    if let Some(id) = content.body {
                        work.push(Task::Body(id));
                    }
                    work.push(Task::Newlines(content.leading));
                }
                Task::Newlines(n) => {
                    for _ in 0..n {
                        out.push('\n');
                        for prefix in &prefixes {
                            out.push_str(prefix);
                        }
                    }
                }
                Task::PopPrefix => {
                    prefixes.pop();
                }
                Task::Body(id) => match &self.nodes[id].part {
                    Part::Released => unreachable!("consumed rope subtree"),
                    Part::Text(text) | Part::TextPartial(text, _) => {
                        for line in text.split_inclusive('\n') {
                            out.push_str(line);
                            if line.ends_with('\n') {
                                for prefix in &prefixes {
                                    out.push_str(prefix);
                                }
                            }
                        }
                    }
                    Part::Repeat(text, n) => {
                        for _ in 0..*n {
                            out.push_str(text);
                        }
                    }
                    Part::Pair(left, right) => {
                        work.push(Task::Content(*right));
                        work.push(Task::Content(*left));
                    }
                    Part::Indent(id, prefix) => {
                        prefixes.push(prefix);
                        work.push(Task::PopPrefix);
                        work.push(Task::Body(*id));
                    }
                },
            }
        }
        debug_assert_eq!(out.len(), self.size(content).bytes);
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn string_budget_checks_boundaries_before_materialization() {
        // All huge values below are symbolic: this test allocates only bytes.
        assert!(check_length(MAX_STRING_LENGTH).is_ok());
        assert_eq!(
            check_length(MAX_STRING_LENGTH + 1).unwrap_err(),
            STRING_LIMIT_ERROR
        );
        let mut rope = Rope::default();
        let max = rope.repeat("=", MAX_STRING_LENGTH).unwrap();
        assert_eq!(rope.utf16_len(max), MAX_STRING_LENGTH);
        let one = rope.text("x").unwrap();
        assert_eq!(rope.concat(max, one).err().unwrap(), STRING_LIMIT_ERROR);
        assert_eq!(
            rope.repeat("=", MAX_STRING_LENGTH + 1).err().unwrap(),
            STRING_LIMIT_ERROR
        );
        assert_eq!(
            rope.flatten(max.append_newlines(1)).unwrap_err(),
            STRING_LIMIT_ERROR
        );
        assert_eq!(
            rope.indent(max.append_newlines(1), "> ").err().unwrap(),
            STRING_LIMIT_ERROR
        );
        assert!(rope.peak_text_bytes() < 100);
    }

    #[test]
    fn budget_counts_utf16_including_non_ascii_and_indented_newlines() {
        let mut rope = Rope::with_limit(13);
        let text = rope.text("中😀\nx").unwrap(); // 9 UTF-8 bytes, 5 UTF-16 units
        assert_eq!(rope.utf16_len(text), 5);
        let text = rope
            .indent(text.prepend_newlines(1).append_newlines(1), "😀")
            .unwrap();
        assert_eq!(rope.utf16_len(text), 13); // 5 + 2 newlines + 3 * 2 prefix units
        let flat = rope.flatten(text).unwrap();
        assert_eq!(flat.encode_utf16().count(), 13);
        assert_eq!(flat, "\n😀中😀\n😀x\n😀");
        assert_eq!(rope.concat(text, text).err().unwrap(), STRING_LIMIT_ERROR);
    }

    fn trim_reference(text: &str) -> String {
        text.trim_start_matches(['\t', '\r', '\n'])
            .trim_end_matches(js_space)
            .to_string()
    }

    #[test]
    fn bounded_trim_matches_materialized_trim_with_indent() {
        let cases = [
            "",
            " ",
            "  \n  ",
            "\n\n",
            "a  \n ",
            "a\n\n   ",
            "a\nb\t\r\n  ",
            "a  \n \n   ",
        ];
        for case in cases {
            for prefix in ["", "> ", "    ", "1.  "] {
                let mut rope = Rope::default();
                let content = rope.text(case).unwrap();
                let content = if prefix.is_empty() {
                    content
                } else {
                    rope.indent(content, prefix).unwrap()
                };
                let full = rope.flatten(content).unwrap();
                let expected = trim_reference(&full);
                let (text, units, bytes, complete) = rope.flatten_bounded(content, 1_000).unwrap();
                assert_eq!(text, expected, "case={case:?} prefix={prefix:?}");
                assert_eq!(units, expected.encode_utf16().count());
                assert_eq!(bytes, expected.len());
                assert!(complete);
            }
        }
    }

    #[test]
    fn nbsp_only_blockquote_trim_matches_real_ts() {
        // Real TS: `<blockquote>&nbsp;<img src=""></blockquote>` -> ">" (1
        // unit/byte). Before the `Size::indent` fix the bounded path returned
        // "> " because the uninserted "> " prefix was treated as body text.
        for case in ["\u{a0}", "\u{a0} ", " \u{a0}"] {
            let mut rope = Rope::default();
            let prefix = rope.text("> ").unwrap();
            let inner = rope.text(case).unwrap();
            let body = rope.indent(inner, "> ").unwrap();
            let content = rope.concat(prefix, body).unwrap();
            let full = rope.flatten(content).unwrap();
            let expected = trim_reference(&full);
            assert_eq!(expected, ">", "case={case:?}");
            let (text, units, bytes, complete) = rope.flatten_bounded(content, 1_000).unwrap();
            assert_eq!((text.as_str(), units, bytes, complete), (">", 1, 1, true));
        }
    }

    #[test]
    fn bounded_byte_length_matches_materialized_multi_byte_output() {
        for case in ["中😀  \n ", "a中\n b😀  ", "😀\t\r\n中"] {
            for prefix in ["> ", "😀 "] {
                let mut rope = Rope::default();
                let content = rope.text(case).unwrap();
                let content = rope.indent(content, prefix).unwrap();
                let content = rope.indent(content, "    ").unwrap();
                let full = rope.flatten(content).unwrap();
                let expected = trim_reference(&full);
                let (text, units, bytes, complete) = rope.flatten_bounded(content, 1_000).unwrap();
                assert_eq!(text, expected, "case={case:?} prefix={prefix:?}");
                assert_eq!(units, expected.encode_utf16().count());
                assert_eq!(bytes, expected.len());
                assert!(complete);
            }
        }

        let mut rope = Rope::default();
        let content = rope.repeat("中😀", 100).unwrap().prepend_newlines(1);
        let content = rope.indent(content, "> ").unwrap();
        let full = rope.flatten(content).unwrap();
        let expected = trim_reference(&full);
        let (text, units, bytes, complete) = rope.flatten_bounded(content, 17).unwrap();
        assert!(!complete);
        assert_eq!(units, expected.encode_utf16().count());
        assert_eq!(bytes, expected.len());
        assert!(text.encode_utf16().count() >= 16);
        assert!(
            expected
                .encode_utf16()
                .collect::<Vec<_>>()
                .starts_with(&text.encode_utf16().collect::<Vec<_>>())
        );
    }

    #[test]
    fn bounded_flatten_stops_at_prefix_units() {
        let mut rope = Rope::default();
        let content = rope.repeat("x", 1_000_000).unwrap().prepend_newlines(1);
        let content = rope.indent(content, "> ").unwrap();
        let (text, units, _bytes, complete) = rope.flatten_bounded(content, 100_001).unwrap();
        assert!(!complete);
        assert_eq!(units, 1_000_002);
        assert_eq!(text.encode_utf16().count(), 100_001);
        // Capacity is bounded by the requested prefix (plus the leading newline
        // and one possible surrogate overshoot), not the symbolic output.
        assert!(rope.peak_flatten_bytes() <= (100_001 + 2) * 4);
        // Grok r3 P1: the visit counter makes "materialize the prefix" a
        // deterministic work bound too. Removing the early stop keeps the same
        // output but walks all 1,000,002 units.
        assert!(
            rope.peak_span_visits() <= 2 * (100_001 + 2),
            "visited {} chars for a 100,001-unit prefix",
            rope.peak_span_visits()
        );
    }

    #[test]
    fn bounded_inline_code_folds_crlf_like_turndown() {
        // Turndown: `content.replace(/\r?\n|\r/g, " ")` — one space per match.
        // CR LF is one match, even when an `&#13;` entity and the literal
        // newline arrive as two separate text nodes. `Some(index)` splits the
        // pair into two nodes with a node boundary between CR and LF.
        let cases: &[(&str, Option<usize>)] = &[
            ("aaaa\r\nbbbb", None),
            ("aaaa\r\nbbbb", Some(4)),
            ("a\r\nb\rc\nd\r\n\re", None),
            ("a\r\nb\rc\nd\r\n\re", Some(1)),
            ("a\r\n\r\nb", Some(3)),
            ("a\rb\nc", None),
            ("\r\n", Some(0)),
            ("a\r\n", None),
        ];
        for &(case, split) in cases {
            let build = |rope: &mut Rope| {
                if let Some(index) = split {
                    assert_eq!(&case[index..index + 2], "\r\n", "case={case:?}");
                    let left = rope.text(&case[..index + 1]).unwrap();
                    let right = rope.text(&case[index + 1..]).unwrap();
                    rope.concat(left, right).unwrap()
                } else {
                    rope.text(case).unwrap()
                }
            };
            let expected = {
                let mut rope = Rope::default();
                let content = build(&mut rope);
                inline_code(rope.flatten(content).unwrap()).unwrap()
            };
            let expected_units = expected.encode_utf16().count();
            for k in 0..=expected_units + 1 {
                let mut rope = Rope::default();
                // Cap the window at `k` so `inline_code` takes the bounded
                // assembly (`need = prefix_units + 4`) instead of the full
                // flatten; otherwise this test only re-checks `inline_code`.
                rope.set_prefix_units(k);
                let content = build(&mut rope);
                let code = rope.inline_code(content).unwrap();
                let (text, units, bytes, complete) = rope.flatten_bounded(code, k).unwrap();
                assert_eq!(units, expected_units, "case={case:?} k={k}: full length");
                assert_eq!(bytes, expected.len(), "case={case:?} k={k}: byte length");
                assert_eq!(
                    complete,
                    expected_units <= k,
                    "case={case:?} k={k}: completeness"
                );
                assert!(
                    expected.starts_with(&text),
                    "case={case:?} k={k}: {text:?} is not a prefix of {expected:?}"
                );
                let got = text.encode_utf16().count();
                assert!(
                    got >= k.min(expected_units) && got <= k.min(expected_units) + 1,
                    "case={case:?} k={k}: materialized {got} units"
                );
            }
        }
    }

    #[test]
    fn partial_seam_does_not_fold_a_trailing_cr_into_the_next_chunk() {
        // A `TextPartial` only keeps a prefix; its tail sits between that
        // prefix and the next chunk in the logical stream. A trailing CR in
        // the prefix must therefore not pair with the next chunk's leading LF:
        // Turndown's `/\r?\n|\r/g` sees the tail's first character in
        // between, so the CR and the LF are two separate matches (two spaces).
        // `analyze_inline_code_span` resets `last_pushed` at the same seam.
        let mut rope = Rope::default();
        let partial = rope
            .text_partial(
                "a\r".into(),
                Size {
                    utf16: 8,
                    bytes: 8,
                    ..Size::default()
                },
                CodeShape::default(),
            )
            .unwrap();
        let next = rope.text("\nz").unwrap();
        let content = rope.concat(partial, next).unwrap();
        let text = rope.flatten_span(content, 0, 8, true).unwrap();
        assert_eq!(text, "a  z");
    }

    #[test]
    fn bounded_inline_code_covers_a_trimmed_crlf_prefix() {
        // Grok r4 P1: a flank trim materializes a raw-unit prefix, but an
        // enclosing `<code>` counts folded units. Eight CR LF pairs in the
        // prefix make a `need`-unit prefix fold to fewer than `need` units, so
        // without growing it the bounded walk runs past the prefix end and
        // skips the unmaterialized tail (wrong bytes at k=20).
        let body = format!("a{}cccccc\r{}", "\r\n".repeat(8), "b".repeat(20));
        let span = |rope: &mut Rope| {
            let content = rope.text(&format!(" {body} ")).unwrap();
            rope.trim_content(content).unwrap()
        };
        let expected = {
            let mut rope = Rope::default();
            let span = span(&mut rope);
            let sibling = rope.text("\nz").unwrap();
            let content = rope.concat(span, sibling).unwrap();
            let code = rope.inline_code(content).unwrap();
            rope.flatten(code).unwrap()
        };
        let expected_units = expected.encode_utf16().count();
        assert_eq!(expected_units, 40);
        for k in 0..=expected_units {
            let mut rope = Rope::default();
            rope.set_prefix_units(k);
            let span = span(&mut rope);
            let sibling = rope.text("\nz").unwrap();
            let content = rope.concat(span, sibling).unwrap();
            let code = rope.inline_code(content).unwrap();
            let (text, units, _bytes, _complete) = rope.flatten_bounded(code, k).unwrap();
            assert_eq!(units, expected_units, "k={k}: full length");
            assert!(
                expected.starts_with(&text),
                "k={k}: {text:?} is not a prefix of {expected:?}"
            );
            let got = text.encode_utf16().count();
            assert!(
                got >= k.min(expected_units) && got <= k.min(expected_units) + 1,
                "k={k}: materialized {got} units"
            );
        }
    }
}
