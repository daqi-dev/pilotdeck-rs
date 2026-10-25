//! Domino 2.2.0 compatibility at the parser boundary.
//!
//! html5ever 0.40.1 implements the current HTML tree-building rules. Two rules
//! moved after domino 2.2.0 (the fixed TS baseline) and make `web_fetch`
//! HTML→Markdown differ on shallow input:
//!
//! * the old `in select` insertion mode ignored every token inside `<select>`
//!   except `option`/`optgroup`, text, comments, `script`/`template`, and
//!   `input`/`keygen`/`textarea` (which closed the select); the current spec
//!   removed that mode (customizable select), so html5ever keeps block markup;
//! * `<search>` is an ordinary unknown inline element in domino, while the
//!   current spec added it to the p-closing block set.
//!
//! Neither can be expressed through `TreeSink`: insertion modes live inside
//! html5ever. This module rewrites only those two constructs before parsing,
//! modelling the old `in select`/`in select in table` rules over the token
//! stream. Everything else is copied byte-for-byte, and documents without
//! `<select` / `<search` take the borrowed fast path.
//!
//! The scanner only models the tokenizer/tree-building states it names. Some
//! constructs change tokenization or the select context in ways it does not
//! model: foreign content (`<svg>` / `<math>` roots, including their
//! integration points), `PLAINTEXT` (which has no end tag), and script data
//! escaped/double-escaped regions. At the first such construct the remainder
//! is handed to html5ever verbatim, without dropping tags or renaming
//! `<search>`. That is deliberately conservative: html5ever's current rules
//! may differ from domino after that point; those differences are registered
//! in `specs/gateway-parity/diffs/A6.5-WEBFETCH-HTMLCOMPAT-BAILOUT-001.md`
//! instead of being modelled token by token.
use std::borrow::Cow;

/// Neutral inline element name that replaces `<search>`. It is not in
/// Turndown's block list and not in the current HTML p-closing set, so its
/// rendering is the same empty inline wrapper domino produced for `search`.
const SEARCH_ALIAS: &str = "x-pd-search";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Context {
    Select,
    Template,
}

struct Frame {
    context: Context,
    /// Table scope is a separate parsing context inside `<template>`.
    saved_table_depth: usize,
}

/// The open frame stack plus a per-context frame count.
///
/// The old `in select` mode can emit arbitrarily many unmatched end tags. A
/// linear existence scan over the whole stack on every such tag made a deep
/// stack of `<template>` frames followed by `</select>` tags quadratic, so
/// existence is answered from the counts instead. `pop_to` still pops every
/// frame above the target, but each frame is pushed and popped at most once,
/// so the total work stays linear in the token count.
struct FrameStack {
    frames: Vec<Frame>,
    select_frames: usize,
    template_frames: usize,
}

impl FrameStack {
    fn new() -> Self {
        Self {
            frames: Vec::new(),
            select_frames: 0,
            template_frames: 0,
        }
    }

    fn last(&self) -> Option<&Frame> {
        self.frames.last()
    }

    fn contains(&self, context: Context) -> bool {
        match context {
            Context::Select => self.select_frames > 0,
            Context::Template => self.template_frames > 0,
        }
    }

    fn push(&mut self, frame: Frame) {
        match frame.context {
            Context::Select => self.select_frames += 1,
            Context::Template => self.template_frames += 1,
        }
        self.frames.push(frame);
    }

    fn pop(&mut self) -> Option<Frame> {
        let frame = self.frames.pop()?;
        match frame.context {
            Context::Select => self.select_frames -= 1,
            Context::Template => self.template_frames -= 1,
        }
        Some(frame)
    }

    /// Pop through the nearest frame of `wanted`, restoring its saved table
    /// depth. An unmatched end tag changes nothing in the old insertion modes.
    fn pop_to(&mut self, wanted: Context, table_depth: &mut usize) {
        if !self.contains(wanted) {
            return;
        }
        while let Some(frame) = self.pop() {
            if frame.context == wanted {
                *table_depth = frame.saved_table_depth;
                return;
            }
        }
    }
}

/// Rewrite the two parser-version gaps to domino 2.2.0 behaviour.
pub(crate) fn domino_compatible(html: &str) -> Cow<'_, str> {
    if !contains_ci(html.as_bytes(), b"<select") && !contains_ci(html.as_bytes(), b"<search") {
        return Cow::Borrowed(html);
    }
    let bytes = html.as_bytes();
    let mut out = String::with_capacity(html.len());
    let mut frames = FrameStack::new();
    let mut table_depth = 0usize;
    let mut i = 0usize;
    while i < bytes.len() {
        let Some(offset) = bytes[i..].iter().position(|&byte| byte == b'<') else {
            out.push_str(&html[i..]);
            break;
        };
        if offset > 0 {
            out.push_str(&html[i..i + offset]);
            i += offset;
        }
        let rest = &html[i..];
        if let Some(comment) = rest.strip_prefix("<!--") {
            // `<!-->` and `<!--->` are abrupt closings of an empty comment.
            let end = if comment.starts_with('>') {
                i + 5
            } else if comment.starts_with("->") {
                i + 6
            } else {
                i + 4 + comment_end(comment)
            };
            out.push_str(&html[i..end]);
            i = end;
            continue;
        }
        if rest.starts_with("<![CDATA[") {
            // In HTML content this is a bogus comment ending at the first
            // `>`; foreign-content CDATA is outside the modelled scope.
            let end = scan_bogus_comment_end(bytes, i + 9);
            out.push_str(&html[i..end]);
            i = end;
            continue;
        }
        if matches!(bytes.get(i + 1), Some(b'!') | Some(b'?')) {
            let end = scan_bogus_comment_end(bytes, i + 2);
            out.push_str(&html[i..end]);
            i = end;
            continue;
        }

        let is_end = bytes.get(i + 1) == Some(&b'/');
        let name_start = i + if is_end { 2 } else { 1 };
        let mut name_end = name_start;
        while name_end < bytes.len() && !is_tag_name_delimiter(bytes[name_end]) {
            name_end += 1;
        }
        if name_end == name_start {
            // A lone `<` that does not begin a tag is text.
            out.push('<');
            i += 1;
            continue;
        }
        let name = &html[name_start..name_end];
        let (tag_end, self_closing) = scan_tag_end(bytes, name_end);
        if bytes.get(tag_end - 1) != Some(&b'>') {
            // The tokenizer emits no tag for an unterminated tag: keep the
            // remaining bytes for the parser instead of modelling a tag.
            out.push_str(&html[i..]);
            break;
        }
        let tag = &html[i..tag_end];
        let in_select = frames
            .last()
            .is_some_and(|frame| frame.context == Context::Select);
        let in_table = table_depth > 0;

        // Unmodelled structure: stop rewriting and let html5ever parse the
        // rest verbatim. Doing this before any tag is emitted also covers the
        // script start tag whose own raw text is escaped.
        if unmodelled(name, is_end, self_closing, tag_end, in_select, html) {
            out.push_str(&html[i..]);
            break;
        }

        if in_select {
            if is_end {
                if eq(name, "select") {
                    out.push_str(tag);
                    frames.pop_to(Context::Select, &mut table_depth);
                } else if eq(name, "option") || eq(name, "optgroup") || eq(name, "script") {
                    // `</script>` must survive to terminate raw text; the
                    // other two are the only kept end tags in the old mode.
                    out.push_str(tag);
                } else if eq(name, "template") {
                    out.push_str(tag);
                    frames.pop_to(Context::Template, &mut table_depth);
                } else if in_table && closes_select_in_table(name) {
                    // in select in table: close the select and reprocess.
                    close_select(&mut out, &mut frames, &mut table_depth);
                    out.push_str(tag);
                    leave_table_element(name, &mut table_depth);
                }
                // Everything else is ignored by the old in-select mode.
            } else if eq(name, "select") {
                // A nested start tag is treated as the select's end tag and is
                // not inserted.
                close_select(&mut out, &mut frames, &mut table_depth);
            } else if eq(name, "option") || eq(name, "optgroup") {
                out.push_str(tag);
            } else if eq(name, "input") || eq(name, "keygen") || eq(name, "textarea") {
                close_select(&mut out, &mut frames, &mut table_depth);
                out.push_str(tag);
                if eq(name, "textarea") {
                    i = copy_raw_text(html, tag_end, "textarea", &mut out);
                    continue;
                }
            } else if eq(name, "script") {
                out.push_str(tag);
                i = copy_raw_text(html, tag_end, "script", &mut out);
                continue;
            } else if eq(name, "template") {
                out.push_str(tag);
                frames.push(Frame {
                    context: Context::Template,
                    saved_table_depth: table_depth,
                });
                table_depth = 0;
            } else if eq(name, "hr") {
                // Domino's old in-select mode ignores HR; the current spec
                // allows it as a select child.
            } else if in_table && closes_select_in_table(name) {
                close_select(&mut out, &mut frames, &mut table_depth);
                out.push_str(tag);
                enter_table_element(name, &mut table_depth);
            }
            // Everything else is ignored by the old in-select mode.
        } else {
            if eq(name, "search") {
                if is_end {
                    out.push_str("</");
                    out.push_str(SEARCH_ALIAS);
                } else {
                    out.push('<');
                    out.push_str(SEARCH_ALIAS);
                }
                out.push_str(&tag[name_end - i..]);
            } else {
                out.push_str(tag);
                if !is_end && eq(name, "select") {
                    frames.push(Frame {
                        context: Context::Select,
                        saved_table_depth: table_depth,
                    });
                } else if !is_end && eq(name, "template") {
                    frames.push(Frame {
                        context: Context::Template,
                        saved_table_depth: table_depth,
                    });
                    table_depth = 0;
                } else if is_end && eq(name, "template") {
                    frames.pop_to(Context::Template, &mut table_depth);
                } else if in_table && is_end && eq(name, "table") {
                    leave_table_element(name, &mut table_depth);
                } else if !is_end && eq(name, "table") {
                    enter_table_element(name, &mut table_depth);
                }
            }
            if !is_end && let Some(end_name) = raw_text_element(name) {
                i = copy_raw_text(html, tag_end, end_name, &mut out);
                continue;
            }
        }
        i = tag_end;
    }
    Cow::Owned(out)
}

fn close_select(out: &mut String, frames: &mut FrameStack, table_depth: &mut usize) {
    out.push_str("</select>");
    frames.pop_to(Context::Select, table_depth);
}

/// True for the constructs whose tokenization or select context this scanner
/// does not model. Called before the tag is emitted: the caller then appends
/// the untouched remainder and stops rewriting.
fn unmodelled(
    name: &str,
    is_end: bool,
    self_closing: bool,
    tag_end: usize,
    in_select: bool,
    html: &str,
) -> bool {
    if is_end {
        return false;
    }
    if !in_select {
        // A self-closing foreign root is popped immediately, so no foreign
        // subtree follows; later tokens are ordinary HTML again.
        if (eq(name, "svg") || eq(name, "math")) && !self_closing {
            return true;
        }
        // PLAINTEXT has no end tag: every following byte is text.
        if eq(name, "plaintext") {
            return true;
        }
    }
    // Old in-select keeps script and the tokenizer still enters escaped
    // states, so this is unmodelled in either context.
    eq(name, "script") && script_enters_double_escape(html, tag_end)
}

/// Offset of `</name` from `from`, or the input length when it is missing.
fn raw_text_end(bytes: &[u8], from: usize, end_name: &str) -> usize {
    let needle = format!("</{end_name}");
    let mut search = from;
    while search < bytes.len() {
        let Some(offset) = bytes[search..].iter().position(|&byte| byte == b'<') else {
            break;
        };
        let at = search + offset;
        if at + needle.len() <= bytes.len()
            && bytes[at..at + needle.len()].eq_ignore_ascii_case(needle.as_bytes())
            && bytes
                .get(at + needle.len())
                .is_none_or(|byte| is_tag_name_delimiter(*byte))
        {
            return at;
        }
        search = at + 1;
    }
    bytes.len()
}

/// Copy raw text up to (not including) `</name`; the main loop then processes
/// the end tag. If it is missing, the remainder is raw text.
fn copy_raw_text(html: &str, from: usize, end_name: &str, out: &mut String) -> usize {
    let end = raw_text_end(html.as_bytes(), from, end_name);
    out.push_str(&html[from..end]);
    end
}

/// True when script data contains `<!--` followed by a `<script` name that
/// enters the double-escaped state before `-->` closes the escaped region.
/// Only that state makes the first `</script` non-closing; a plain `<!--`
/// opener (even unterminated) does not move our raw-scan end, so it stays
/// modelled and is not a reason to stop rewriting.
fn script_enters_double_escape(html: &str, from: usize) -> bool {
    let bytes = html.as_bytes();
    let end = raw_text_end(bytes, from, "script");
    let mut search = from;
    while search + 4 <= end {
        let Some(offset) = bytes[search..end]
            .windows(4)
            .position(|window| window == b"<!--")
        else {
            return false;
        };
        let mut cursor = search + offset + 4;
        loop {
            if cursor >= end {
                return false;
            }
            if bytes[cursor..end].starts_with(b"-->") {
                search = cursor + 3;
                break;
            }
            if cursor + 7 <= end
                && bytes[cursor..cursor + 7].eq_ignore_ascii_case(b"<script")
                && bytes
                    .get(cursor + 7)
                    .is_some_and(|byte| is_tag_name_delimiter(*byte))
            {
                return true;
            }
            cursor += 1;
        }
    }
    false
}

fn raw_text_element(name: &str) -> Option<&'static str> {
    [
        "script", "style", "textarea", "title", "xmp", "iframe", "noembed", "noframes",
    ]
    .into_iter()
    .find(|&element| eq(name, element))
}

/// Table scope is established only by a `<table>` element. The old scanner
/// counted every table-structure tag, so an omitted `</td>`/`</tr>` (or a
/// single `</table>` popping the implicit row/cell stack) left the depth above
/// zero after the table closed. `tr`/`td`/`th` and the other structure tags do
/// not create table scope and are ignored here; `</table>` closes the nearest
/// open table.
fn enter_table_element(name: &str, table_depth: &mut usize) {
    if eq(name, "table") {
        *table_depth += 1;
    }
}

fn leave_table_element(name: &str, table_depth: &mut usize) {
    if eq(name, "table") {
        *table_depth = table_depth.saturating_sub(1);
    }
}

/// The old `in select in table` close list: caption, table, tbody, tfoot,
/// thead, tr, td, th. `colgroup`/`col` are absent: the old in-select mode
/// ignores them instead of closing the select.
fn closes_select_in_table(name: &str) -> bool {
    for element in [
        "caption", "table", "tbody", "tfoot", "thead", "tr", "td", "th",
    ] {
        if eq(name, element) {
            return true;
        }
    }
    false
}

fn eq(name: &str, expected: &str) -> bool {
    name.eq_ignore_ascii_case(expected)
}

fn is_tag_name_delimiter(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\n' | b'\r' | b'\x0c' | b'/' | b'>')
}

fn is_html_space(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\n' | b'\r' | b'\x0c')
}

/// Scan a start or end tag from `from`, ending after its `>`.
///
/// Quotes only hide `>` while they delimit an attribute value: a quote in a
/// name, or inside an unquoted value, is ordinary data in the tokenizer.
fn scan_tag_end(bytes: &[u8], from: usize) -> (usize, bool) {
    #[derive(Clone, Copy)]
    enum State {
        BeforeAttributeName,
        AttributeName,
        AfterAttributeName,
        BeforeAttributeValue,
        UnquotedValue,
        QuotedValue(u8),
        AfterQuotedValue,
        SelfClosing,
    }

    let mut state = State::BeforeAttributeName;
    let mut index = from;
    while index < bytes.len() {
        let byte = bytes[index];
        match state {
            State::BeforeAttributeName => match byte {
                b'>' => return (index + 1, false),
                b'/' => state = State::SelfClosing,
                byte if is_html_space(byte) => {}
                _ => state = State::AttributeName,
            },
            State::AttributeName => match byte {
                b'>' => return (index + 1, false),
                b'/' => state = State::SelfClosing,
                b'=' => state = State::BeforeAttributeValue,
                byte if is_html_space(byte) => state = State::AfterAttributeName,
                _ => {}
            },
            State::AfterAttributeName => match byte {
                b'>' => return (index + 1, false),
                b'/' => state = State::SelfClosing,
                b'=' => state = State::BeforeAttributeValue,
                byte if is_html_space(byte) => {}
                _ => state = State::AttributeName,
            },
            State::BeforeAttributeValue => match byte {
                b'>' => return (index + 1, false),
                b'"' => state = State::QuotedValue(b'"'),
                b'\'' => state = State::QuotedValue(b'\''),
                byte if is_html_space(byte) => {}
                _ => state = State::UnquotedValue,
            },
            State::UnquotedValue => match byte {
                b'>' => return (index + 1, false),
                byte if is_html_space(byte) => state = State::BeforeAttributeName,
                _ => {}
            },
            State::QuotedValue(wanted) => {
                if byte == wanted {
                    state = State::AfterQuotedValue;
                }
            }
            State::AfterQuotedValue => match byte {
                b'>' => return (index + 1, false),
                b'/' => state = State::SelfClosing,
                byte if is_html_space(byte) => state = State::BeforeAttributeName,
                _ => {
                    state = State::BeforeAttributeName;
                    continue;
                }
            },
            State::SelfClosing => {
                if byte == b'>' {
                    return (index + 1, true);
                }
                state = State::BeforeAttributeName;
                continue;
            }
        }
        index += 1;
    }
    (bytes.len(), false)
}

/// A bogus comment (`<!x`, `<?`, or `<![CDATA[` in HTML content) ends at the
/// first `>`; quotes are not special there.
fn scan_bogus_comment_end(bytes: &[u8], from: usize) -> usize {
    bytes[from..]
        .iter()
        .position(|&byte| byte == b'>')
        .map_or(bytes.len(), |offset| from + offset + 1)
}

/// Offset just past the comment terminator (`-->` or `--!>`), or the comment
/// length when it is unterminated.
fn comment_end(comment: &str) -> usize {
    let bytes = comment.as_bytes();
    let mut index = 0;
    while index + 2 < bytes.len() {
        if bytes[index] == b'-' && bytes[index + 1] == b'-' {
            if bytes[index + 2] == b'>' {
                return index + 3;
            }
            if bytes[index + 2] == b'!' && bytes.get(index + 3) == Some(&b'>') {
                return index + 4;
            }
        }
        index += 1;
    }
    bytes.len()
}

fn contains_ci(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window.eq_ignore_ascii_case(needle))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rewritten(html: &str) -> String {
        domino_compatible(html).into_owned()
    }

    #[test]
    fn leaves_untouched_documents_borrowed() {
        let html = "<p>hello</p><script>\"plain\"</script>";
        assert!(matches!(domino_compatible(html), Cow::Borrowed(_)));
        // A script literal still triggers the slow path, but its bytes survive.
        let literal = "<p>hello</p><script>\"<select>\"</script>";
        assert_eq!(rewritten(literal), literal);
    }

    #[test]
    fn drops_old_in_select_markup_and_keeps_allowed_tokens() {
        assert_eq!(rewritten("<select><hr></select>"), "<select></select>");
        assert_eq!(
            rewritten("<select><div><p>x</p></div></select>"),
            "<select>x</select>"
        );
        assert_eq!(
            rewritten("<select><option><div>b</div></option></select>"),
            "<select><option>b</option></select>"
        );
        assert_eq!(
            rewritten("<select><script>var x = \"<select>\";</script>b</select>"),
            "<select><script>var x = \"<select>\";</script>b</select>"
        );
        assert_eq!(
            rewritten("<select><!--><div>b</div></select>"),
            "<select><!-->b</select>"
        );
        assert_eq!(
            rewritten("<select>a<textarea>b</textarea>c</select>"),
            "<select>a</select><textarea>b</textarea>c</select>"
        );
        assert_eq!(
            rewritten("<select>a<select>b</select>c</select>"),
            "<select>a</select>b</select>c</select>"
        );
    }

    #[test]
    fn search_becomes_a_neutral_inline_element() {
        assert_eq!(
            rewritten("<p>a<search>b</search>c</p>"),
            "<p>a<x-pd-search>b</x-pd-search>c</p>"
        );
        assert_eq!(
            rewritten("<SEARCH data-x='<search>'>b</SEARCH>"),
            "<x-pd-search data-x='<search>'>b</x-pd-search>"
        );
    }

    #[test]
    fn select_in_table_closes_before_structure_tags() {
        assert_eq!(
            rewritten("<table><tr><td><select><tr><td>b</table>"),
            "<table><tr><td><select></select><tr><td>b</table>"
        );
    }

    #[test]
    fn table_depth_tracks_only_open_table_elements() {
        // Omitted </td></tr>: the single </table> closes the table scope, so
        // the following body select is not in-select-in-table and its table
        // tags are ignored by the old select mode.
        assert_eq!(
            rewritten(
                "<table><tr><td>a</table><select>b<table><tr><td>c</td></tr></table>d</select>"
            ),
            "<table><tr><td>a</table><select>bcd</select>"
        );
        // Unmatched </td> end tags must not decrement the still-open table.
        assert_eq!(
            rewritten(
                "<table><tr><td>a</td></td></td><select>x<table>y</table>z</select></tr></table>"
            ),
            "<table><tr><td>a</td></td></td><select>x</select><table>y</table>z</select></tr></table>"
        );
        assert_eq!(
            rewritten("<table></td></tr><select>x<table>y</table>z</select></table>"),
            "<table></td></tr><select>x</select><table>y</table>z</select></table>"
        );
        // Balanced controls keep their previous rewrite.
        assert_eq!(
            rewritten(
                "<table><tr><td>a</td></tr></table><select>b<table><tr><td>c</td></tr></table>d</select>"
            ),
            "<table><tr><td>a</td></tr></table><select>bcd</select>"
        );
        // A real nested table inside a cell leaves the outer table scope open:
        // clearing the depth on every </table> would ignore the second table.
        assert_eq!(
            rewritten(
                "<table><tr><td><table><tr><td>x</td></tr></table><select>y<table><tr><td>z</td></tr></table>w</select></td></tr></table>"
            ),
            "<table><tr><td><table><tr><td>x</td></tr></table><select>y</select><table><tr><td>z</td></tr></table>w</select></td></tr></table>"
        );
    }

    #[test]
    fn quotes_only_open_attribute_values() {
        // A quote inside an unquoted value is ordinary value data, so the tag
        // still ends at `>` and the text after it survives.
        assert_eq!(
            rewritten("<select><div a=b\"c>d</select>"),
            "<select>d</select>"
        );
        assert_eq!(
            rewritten("<select><div a=b'c>d</select>"),
            "<select>d</select>"
        );
        assert_eq!(
            rewritten("<select><div a=b\"c' d>e</select>"),
            "<select>e</select>"
        );
        // Quotes in an attribute name are not quoted values either.
        assert_eq!(
            rewritten("<select><div a\"b c>d</select>"),
            "<select>d</select>"
        );
        // A quote as the first value byte still hides `>`.
        assert_eq!(
            rewritten("<select><div title=\"a>b\">c</div>d</select>"),
            "<select>cd</select>"
        );
    }

    #[test]
    fn unterminated_tags_are_preserved() {
        assert_eq!(rewritten("<select><div a=b"), "<select><div a=b");
    }

    #[test]
    fn bogus_comments_end_at_the_first_gt() {
        assert_eq!(
            rewritten("<? a \" > <select><hr></select>"),
            "<? a \" > <select></select>"
        );
        assert_eq!(
            rewritten("<![CDATA[ \" > <select><hr></select>"),
            "<![CDATA[ \" > <select></select>"
        );
        assert_eq!(
            rewritten("<!x a ' > <select><hr></select>"),
            "<!x a ' > <select></select>"
        );
        assert_eq!(
            rewritten("<!-- x --!><select><hr></select>"),
            "<!-- x --!><select></select>"
        );
    }

    #[test]
    fn foreign_content_hands_the_remainder_to_the_parser() {
        // Breakout tokens inside a foreign <select> are real HTML, not
        // in-select markup to drop.
        assert_eq!(
            rewritten("<svg><select><hr></select></svg>"),
            "<svg><select><hr></select></svg>"
        );
        assert_eq!(
            rewritten("<math><select><h1>x</h1></select></math>"),
            "<math><select><h1>x</h1></select></math>"
        );
        // After the foreign root starts, later HTML is not rewritten either.
        assert_eq!(
            rewritten("<svg></svg><select><hr></select>"),
            "<svg></svg><select><hr></select>"
        );
        // A self-closing foreign root has no foreign subtree, so the
        // following <select> is still ordinary HTML.
        assert_eq!(
            rewritten("<svg/><select><hr></select>"),
            "<svg/><select></select>"
        );
        // A slash separated from `>` does not set the self-closing flag.
        assert_eq!(
            rewritten("<svg / ><select><hr></select>"),
            "<svg / ><select><hr></select>"
        );
        // Nor does a slash inside an unquoted attribute value.
        assert_eq!(
            rewritten("<svg a=b/><select><hr></select>"),
            "<svg a=b/><select><hr></select>"
        );
        // A slash after a quoted value does set it.
        assert_eq!(
            rewritten("<svg a=\"b\"/><select><hr></select>"),
            "<svg a=\"b\"/><select></select>"
        );
        // <svg>/<math> inside <select> are ignored by the old in-select mode,
        // so the select model still applies.
        assert_eq!(
            rewritten("<select><svg><hr></svg><hr></select>"),
            "<select></select>"
        );
    }

    #[test]
    fn plaintext_hands_the_remainder_to_the_parser() {
        // PLAINTEXT has no end tag: `</plaintext>` and every later tag are
        // literal text, so nothing after it may be dropped or renamed.
        assert_eq!(
            rewritten("<p>a<plaintext>b</plaintext><select><hr></select>"),
            "<p>a<plaintext>b</plaintext><select><hr></select>"
        );
        // Inside <select> the start tag is ignored by the old mode, so the
        // tokenizer never enters PLAINTEXT.
        assert_eq!(
            rewritten("<select><plaintext><hr></select>"),
            "<select></select>"
        );
    }

    #[test]
    fn script_double_escape_hands_the_remainder_to_the_parser() {
        // `<!--<script` enters script data double escaped: the first
        // `</script>` does not close the element, so the following <hr> is
        // raw script text and must not be dropped.
        assert_eq!(
            rewritten("<div>a<select>b<script><!--<script></script><hr></script>c</select>d</div>"),
            "<div>a<select>b<script><!--<script></script><hr></script>c</select>d</div>"
        );
        assert_eq!(
            rewritten("<div>a<script><!--<script></script><search>x</search></script>b</div>"),
            "<div>a<script><!--<script></script><search>x</search></script>b</div>"
        );
        // A plain `<!--` (even unterminated) does not move the raw-scan end,
        // so it stays modelled and later <select> markup is still rewritten.
        assert_eq!(
            rewritten("<script>var s = \"<!--\";</script><select><hr></select>"),
            "<script>var s = \"<!--\";</script><select></select>"
        );
        // RAWTEXT (style) has no escaped state: its text is copied verbatim
        // and the next <select> is still rewritten.
        assert_eq!(
            rewritten("<style><!--<script></script></style><select><hr></select>"),
            "<style><!--<script></script></style><select></select>"
        );
    }

    #[test]
    fn colgroup_does_not_close_a_select_in_a_table() {
        // The old `in select in table` close set is caption/table/tbody/
        // tfoot/thead/tr/td/th; colgroup and col are ignored instead.
        assert_eq!(
            rewritten("<table><tr><td><select>a<colgroup><h1>x</h1></select>c</table>"),
            "<table><tr><td><select>ax</select>c</table>"
        );
        assert_eq!(
            rewritten("<table><tr><td><select>a<colgroup><option>o</option></select>c</table>"),
            "<table><tr><td><select>a<option>o</option></select>c</table>"
        );
        assert_eq!(
            rewritten("<table><tr><td><select>a<colgroup><select>b</select>c</table>"),
            "<table><tr><td><select>a</select>b</select>c</table>"
        );
        // A real table-structure token still closes the select.
        assert_eq!(
            rewritten("<table><tr><td><select>a<tbody><tr><td>z</table>"),
            "<table><tr><td><select>a</select><tbody><tr><td>z</table>"
        );
    }

    #[test]
    fn unmatched_end_tags_do_not_pop_outer_frames() {
        assert_eq!(
            rewritten("<div>a<select>b</template><hr>c</select>d</div>"),
            "<div>a<select>b</template>c</select>d</div>"
        );
        assert_eq!(
            rewritten("<div>a<select>b</template><div>x</div>c</select>d</div>"),
            "<div>a<select>b</template>xc</select>d</div>"
        );
        assert_eq!(
            rewritten("<select>b</template><hr>c</select>"),
            "<select>b</template>c</select>"
        );
        // A real template frame still pops through from select to template.
        assert_eq!(
            rewritten("<template><select>b</template><hr>c</select>d"),
            "<template><select>b</template><hr>c</select>d"
        );
        assert_eq!(
            rewritten("<select><template></template><hr>b</select>"),
            "<select><template></template>b</select>"
        );
    }

    /// `templates` open `<template>` frames followed by `<select></select>`
    /// pairs, tiled to exactly `bytes` bytes. Every `</select>` used to scan
    /// the frame stack bottom-up for a select frame, so N templates × M
    /// selects was quadratic.
    fn deep_frame_select_fixture(bytes: usize, templates: usize) -> String {
        const TEMPLATE: &str = "<template>";
        const SELECT_PAIR: &str = "<select></select>";
        let head = TEMPLATE.repeat(templates);
        let pairs = (bytes - head.len()) / SELECT_PAIR.len();
        let html = head + &SELECT_PAIR.repeat(pairs);
        assert_eq!(html.len(), bytes, "fixture must tile exactly");
        html
    }

    fn timed_rewrite(html: &str) -> std::time::Duration {
        let started = std::time::Instant::now();
        let output = domino_compatible(html);
        let elapsed = started.elapsed();
        assert!(
            output.as_bytes() == html.as_bytes(),
            "the deep-frame fixture must be rewritten byte-for-byte"
        );
        elapsed
    }

    #[test]
    fn deep_frame_stack_select_end_tags_stay_within_linear_budget() {
        // GLM r4's shape: 100,000 nested <template> frames and 434,624
        // <select></select> pairs are exactly 8 MiB. Each `</select>` closes
        // the select frame sitting on top of the 100,000 template frames.
        let half = deep_frame_select_fixture(4 * 1024 * 1024, 50_000);
        let full = deep_frame_select_fixture(8 * 1024 * 1024, 100_000);
        let half_elapsed = timed_rewrite(&half);
        let full_elapsed = timed_rewrite(&full);
        eprintln!("deep-frame-select: 4 MiB in {half_elapsed:?}, 8 MiB in {full_elapsed:?}");
        // Calibrated in a debug build (see fix-r4/RESULT.md): the fixed
        // scanner needs well under 0.5 s for 8 MiB on the reference machine,
        // so 2 s keeps >4x headroom for a slower CI host, while the quadratic
        // implementation needs minutes.
        const BUDGET: f64 = 2.0;
        assert!(
            full_elapsed.as_secs_f64() < BUDGET,
            "8 MiB rewrite took {full_elapsed:?}, budget {BUDGET}s"
        );
        // Same shape at half size: linear work may double; allow 3x plus
        // 150 ms for scheduler/allocator noise. A quadratic scan would need
        // ~4x on this doubling and is already outside the band.
        assert!(
            full_elapsed.as_secs_f64() <= half_elapsed.as_secs_f64() * 3.0 + 0.15,
            "8 MiB rewrite {full_elapsed:?} did not scale from 4 MiB {half_elapsed:?}"
        );
    }
}
