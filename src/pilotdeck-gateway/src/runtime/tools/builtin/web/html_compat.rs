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

/// Rewrite the two parser-version gaps to domino 2.2.0 behaviour.
pub(crate) fn domino_compatible(html: &str) -> Cow<'_, str> {
    if !contains_ci(html.as_bytes(), b"<select") && !contains_ci(html.as_bytes(), b"<search") {
        return Cow::Borrowed(html);
    }
    let bytes = html.as_bytes();
    let mut out = String::with_capacity(html.len());
    let mut frames: Vec<Frame> = Vec::new();
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
        let tag_end = scan_tag_end(bytes, name_end);
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

        if in_select {
            if is_end {
                if eq(name, "select") {
                    out.push_str(tag);
                    pop_frame(&mut frames, &mut table_depth, Context::Select);
                } else if eq(name, "option") || eq(name, "optgroup") || eq(name, "script") {
                    // `</script>` must survive to terminate raw text; the
                    // other two are the only kept end tags in the old mode.
                    out.push_str(tag);
                } else if eq(name, "template") {
                    out.push_str(tag);
                    pop_frame(&mut frames, &mut table_depth, Context::Template);
                } else if in_table && is_table_structure(name) {
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
            } else if in_table && is_table_structure(name) {
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
                    pop_frame(&mut frames, &mut table_depth, Context::Template);
                } else if in_table && is_end && is_table_structure(name) {
                    leave_table_element(name, &mut table_depth);
                } else if !is_end && is_table_structure(name) {
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

fn close_select(out: &mut String, frames: &mut Vec<Frame>, table_depth: &mut usize) {
    out.push_str("</select>");
    pop_frame(frames, table_depth, Context::Select);
}

fn pop_frame(frames: &mut Vec<Frame>, table_depth: &mut usize, wanted: Context) {
    if !frames.iter().any(|frame| frame.context == wanted) {
        // An unmatched end tag changes nothing in the old insertion modes.
        return;
    }
    while let Some(frame) = frames.pop() {
        if frame.context == wanted {
            *table_depth = frame.saved_table_depth;
            return;
        }
    }
}

/// Copy raw text up to (not including) `</name`; the main loop then processes
/// the end tag. If it is missing, the remainder is raw text.
fn copy_raw_text(html: &str, from: usize, end_name: &str, out: &mut String) -> usize {
    let bytes = html.as_bytes();
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
            out.push_str(&html[from..at]);
            return at;
        }
        search = at + 1;
    }
    out.push_str(&html[from..]);
    bytes.len()
}

fn raw_text_element(name: &str) -> Option<&'static str> {
    [
        "script",
        "style",
        "textarea",
        "title",
        "xmp",
        "iframe",
        "noembed",
        "noframes",
        "plaintext",
    ]
    .into_iter()
    .find(|&element| eq(name, element))
}

fn enter_table_element(name: &str, table_depth: &mut usize) {
    if eq(name, "table") || *table_depth > 0 {
        *table_depth += 1;
    }
}

fn leave_table_element(name: &str, table_depth: &mut usize) {
    if is_table_structure(name) {
        *table_depth = table_depth.saturating_sub(1);
    }
}

fn is_table_structure(name: &str) -> bool {
    for element in [
        "table", "caption", "colgroup", "tbody", "tfoot", "thead", "tr", "td", "th",
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
fn scan_tag_end(bytes: &[u8], from: usize) -> usize {
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
                b'>' => return index + 1,
                b'/' => state = State::SelfClosing,
                byte if is_html_space(byte) => {}
                _ => state = State::AttributeName,
            },
            State::AttributeName => match byte {
                b'>' => return index + 1,
                b'/' => state = State::SelfClosing,
                b'=' => state = State::BeforeAttributeValue,
                byte if is_html_space(byte) => state = State::AfterAttributeName,
                _ => {}
            },
            State::AfterAttributeName => match byte {
                b'>' => return index + 1,
                b'/' => state = State::SelfClosing,
                b'=' => state = State::BeforeAttributeValue,
                byte if is_html_space(byte) => {}
                _ => state = State::AttributeName,
            },
            State::BeforeAttributeValue => match byte {
                b'>' => return index + 1,
                b'"' => state = State::QuotedValue(b'"'),
                b'\'' => state = State::QuotedValue(b'\''),
                byte if is_html_space(byte) => {}
                _ => state = State::UnquotedValue,
            },
            State::UnquotedValue => match byte {
                b'>' => return index + 1,
                byte if is_html_space(byte) => state = State::BeforeAttributeName,
                _ => {}
            },
            State::QuotedValue(wanted) => {
                if byte == wanted {
                    state = State::AfterQuotedValue;
                }
            }
            State::AfterQuotedValue => match byte {
                b'>' => return index + 1,
                b'/' => state = State::SelfClosing,
                byte if is_html_space(byte) => state = State::BeforeAttributeName,
                _ => {
                    state = State::BeforeAttributeName;
                    continue;
                }
            },
            State::SelfClosing => {
                if byte == b'>' {
                    return index + 1;
                }
                state = State::BeforeAttributeName;
                continue;
            }
        }
        index += 1;
    }
    bytes.len()
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
}
