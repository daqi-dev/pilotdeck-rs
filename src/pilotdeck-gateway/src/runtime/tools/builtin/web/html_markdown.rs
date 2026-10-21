//! HTML conversion for the baseline's **default** Turndown configuration.
//! The parser owns a flat arena; normalization and rendering use iterative
//! walks. No GFM plugin is installed by urlFetcher.ts (tables are plain blocks,
//! code blocks are indented, and script/style text is retained).
use super::{
    fetcher::HtmlToMarkdown,
    html_dom::{Dom, Id, Kind, parse_document},
};
use crate::{
    js_value::{number_string, string_to_number},
    metadata::js_space,
};
use std::ops::Range;
#[path = "html_markdown_buffer.rs"]
mod buffer;
pub use super::html_dom::{DOM_LIMIT_ERROR, MAX_DOM_DEPTH};
use buffer::{Content, Rope, check_length, concatenate, repeat_string};
pub use buffer::{MAX_STRING_LENGTH, STRING_LIMIT_ERROR};

#[derive(Clone, Copy, Debug, Default)]
pub struct Turndown;

const BLOCK: &[&str] = &[
    "ADDRESS",
    "ARTICLE",
    "ASIDE",
    "AUDIO",
    "BLOCKQUOTE",
    "BODY",
    "CANVAS",
    "CENTER",
    "DD",
    "DIR",
    "DIV",
    "DL",
    "DT",
    "FIELDSET",
    "FIGCAPTION",
    "FIGURE",
    "FOOTER",
    "FORM",
    "FRAMESET",
    "H1",
    "H2",
    "H3",
    "H4",
    "H5",
    "H6",
    "HEADER",
    "HGROUP",
    "HR",
    "HTML",
    "ISINDEX",
    "LI",
    "MAIN",
    "MENU",
    "NAV",
    "NOFRAMES",
    "NOSCRIPT",
    "OL",
    "OUTPUT",
    "P",
    "PRE",
    "SECTION",
    "TABLE",
    "TBODY",
    "TD",
    "TFOOT",
    "TH",
    "THEAD",
    "TR",
    "UL",
];
const VOID: &[&str] = &[
    "AREA", "BASE", "BR", "COL", "COMMAND", "EMBED", "HR", "IMG", "INPUT", "KEYGEN", "LINK",
    "META", "PARAM", "SOURCE", "TRACK", "WBR",
];
const MEANINGFUL: &[&str] = &[
    "A", "TABLE", "THEAD", "TBODY", "TFOOT", "TH", "TD", "IFRAME", "SCRIPT", "AUDIO", "VIDEO",
];

fn ascii_space(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\r' | '\n')
}

fn collapse_text(text: &str) -> Result<String, String> {
    // Whitespace normalization only shrinks this validated source string.
    check_length(text.encode_utf16().count())?;
    let mut out = String::with_capacity(text.len());
    let mut space = false;
    for c in text.chars() {
        if ascii_space(c) {
            if !space {
                out.push(' ');
            }
            space = true;
        } else {
            out.push(c);
            space = false;
        }
    }
    Ok(out)
}

/// Visit block boundaries on both entry and exit, leaving PRE subtrees intact.
fn collapse(dom: &mut Dom, root: Id, names: &[String]) -> Result<(), String> {
    let mut work: Vec<_> = dom
        .children(root)
        .into_iter()
        .rev()
        .map(|id| (id, false))
        .collect();
    let mut previous_text: Option<Id> = None;
    let mut protect_space = false;
    while let Some((id, exiting)) = work.pop() {
        if let Kind::Text(text) = &dom.nodes[id].kind {
            let mut text = collapse_text(text)?;
            let follows_space = previous_text.is_none_or(
                |prev| matches!(&dom.nodes[prev].kind, Kind::Text(text) if text.ends_with(' ')),
            );
            if follows_space && !protect_space && text.starts_with(' ') {
                text.remove(0);
            }
            if text.is_empty() {
                dom.remove(id);
            } else {
                dom.set_text(id, text);
                previous_text = Some(id);
            }
            continue;
        }
        if !dom.is_element(id) {
            dom.remove(id);
            continue;
        }
        let name = names[id].as_str();
        if BLOCK.contains(&name) || name == "BR" {
            if let Some(prev) = previous_text
                && let Kind::Text(text) = &mut dom.nodes[prev].kind
                && text.ends_with(' ')
            {
                text.pop();
            }
            previous_text = None;
            protect_space = false;
        } else if VOID.contains(&name) {
            previous_text = None;
            protect_space = true;
        } else if previous_text.is_some() {
            protect_space = false;
        }
        if !exiting && name != "PRE" {
            let children = dom.children(id);
            if !children.is_empty() {
                work.push((id, true));
                work.extend(children.into_iter().rev().map(|child| (child, false)));
            }
        }
    }
    if let Some(prev) = previous_text
        && let Kind::Text(text) = &mut dom.nodes[prev].kind
    {
        if text.ends_with(' ') {
            text.pop();
        }
        if text.is_empty() {
            dom.remove(prev);
        }
    }
    Ok(())
}

#[derive(Default)]
struct Summary {
    text: Option<Range<usize>>,
    nonblank: bool,
    meaningful: bool,
    code: bool,
    ordinal: usize,
    last_element: Option<Id>,
}

/// All descendant text occupies a slice of one buffer, avoiding a textContent
/// allocation per ancestor (especially expensive on deep pages).
struct Render<'a> {
    dom: &'a Dom,
    names: &'a [String],
    summary: Vec<Summary>,
    text: String,
}

impl<'a> Render<'a> {
    fn new(dom: &'a Dom, names: &'a [String], order: &[Id]) -> Result<Self, String> {
        let mut summary: Vec<Summary> = (0..dom.nodes.len()).map(|_| Summary::default()).collect();
        let mut text = String::new();
        let mut utf16 = 0;
        for &id in order {
            summary[id].code =
                names[id] == "CODE" || dom.nodes[id].parent.is_some_and(|p| summary[p].code);
            if let Kind::Text(value) = &dom.nodes[id].kind {
                let start = text.len();
                utf16 += value.encode_utf16().count();
                check_length(utf16)?;
                text.push_str(value);
                summary[id].text = Some(start..text.len());
                summary[id].nonblank = value.chars().any(|c| !js_space(c));
            }
            for (ordinal, child) in dom.element_children(id).into_iter().enumerate() {
                summary[child].ordinal = ordinal;
                summary[id].last_element = Some(child);
            }
        }
        for &id in order.iter().rev() {
            let name = names[id].as_str();
            summary[id].meaningful |= VOID.contains(&name) || MEANINGFUL.contains(&name);
            if let Some(parent) = dom.nodes[id].parent {
                summary[parent].nonblank |= summary[id].nonblank;
                summary[parent].meaningful |= summary[id].meaningful;
                if let Some(range) = summary[id].text.clone() {
                    // Reverse preorder: the rightmost descendant arrives first.
                    summary[parent].text = Some(match &summary[parent].text {
                        Some(existing) => range.start..existing.end,
                        None => range,
                    });
                }
            }
        }
        Ok(Self {
            dom,
            names,
            summary,
            text,
        })
    }

    fn text(&self, id: Id) -> &str {
        self.summary[id].text.clone().map_or("", |r| &self.text[r])
    }

    fn flanks(&self, id: Id) -> (&str, &str) {
        if BLOCK.contains(&self.names[id].as_str()) {
            return ("", "");
        }
        let text = self.text(id);
        let leading_end = text.len() - text.trim_start_matches(js_space).len();
        let rest = &text[leading_end..];
        let trailing_start = rest.trim_end_matches(js_space).len();
        let mut leading = &text[..leading_end];
        let mut trailing = &rest[trailing_start..];
        let flanked = |sibling: Option<Id>, left: bool| {
            sibling.is_some_and(|s| {
                (self.dom.is_text(s)
                    || self.dom.is_element(s) && !BLOCK.contains(&self.names[s].as_str()))
                    && if left {
                        self.text(s).ends_with(' ')
                    } else {
                        self.text(s).starts_with(' ')
                    }
            })
        };
        if flanked(self.dom.nodes[id].prev, true) {
            leading = leading.trim_start_matches(ascii_space);
        }
        if flanked(self.dom.nodes[id].next, false) {
            trailing = trailing.trim_end_matches(ascii_space);
        }
        (leading, trailing)
    }

    fn replace(&self, rope: &mut Rope, id: Id, content: Content) -> Result<Content, String> {
        let name = self.names[id].as_str();
        let lower = name.to_ascii_lowercase();
        let node = &self.dom.nodes[id];
        let block = BLOCK.contains(&name);
        let (leading, trailing) = self.flanks(id);
        let mut content = content;
        if !leading.is_empty() || !trailing.is_empty() {
            let mut text = rope.flatten_and_release(content)?;
            let start = text.len() - text.trim_start_matches(js_space).len();
            let end = text.trim_end_matches(js_space).len();
            text.truncate(end.max(start));
            text.drain(..start);
            content = rope.text_owned(text)?;
        }
        let wrap_block = |rope: &Rope, content: Content| rope.check(content.wrap_block());
        // Domino's getElementsByTagName("IMG"/"A"/...) folds case only on
        // HTML elements. Foreign ancestors can therefore still be blank even
        // when their HTML integration-point descendants contain an image/link.
        let meaningful = VOID.contains(&name)
            || MEANINGFUL.contains(&name)
            || self.dom.is_html(id) && self.summary[id].meaningful;
        let replacement = if !self.summary[id].nonblank && !meaningful {
            if block {
                rope.text("\n\n")?
            } else {
                Content::default()
            }
        } else {
            match lower.as_str() {
                "p" => wrap_block(rope, content)?,
                "br" => rope.text("  \n")?,
                "h1" | "h2" => {
                    let marker = if lower == "h1" { "=" } else { "-" };
                    let count = rope.utf16_len(content);
                    let underline = rope.repeat(marker, count)?.prepend_newlines(1);
                    let heading = rope.concat(content, underline)?;
                    wrap_block(rope, heading)?
                }
                "h3" | "h4" | "h5" | "h6" => {
                    let prefix = rope.text(&concatenate(&[
                        &repeat_string("#", (name.as_bytes()[1] - b'0') as usize)?,
                        " ",
                    ])?)?;
                    let heading = rope.concat(prefix, content)?;
                    wrap_block(rope, heading)?
                }
                "blockquote" => {
                    let content = rope.indent(content.trim_newlines(), "> ")?;
                    let prefix = rope.text("> ")?;
                    let quote = rope.concat(prefix, content)?;
                    wrap_block(rope, quote)?
                }
                "ul" | "ol" => {
                    let nested_last = node.parent.is_some_and(|p| {
                        self.names[p] == "LI" && self.summary[p].last_element == Some(id)
                    });
                    if nested_last {
                        content.prepend_newlines(1)
                    } else {
                        wrap_block(rope, content)?
                    }
                }
                "li" => {
                    let prefix =
                        if let Some(parent) = node.parent.filter(|&p| self.names[p] == "OL") {
                            let start = self.dom.attr(parent, "start").filter(|s| !s.is_empty());
                            let n = start.map_or(1.0, string_to_number)
                                + self.summary[id].ordinal as f64;
                            concatenate(&[&number_string(n), ".  "])?
                        } else {
                            "*   ".to_string()
                        };
                    let paragraph =
                        content.trailing > 0 || content.body.is_none() && content.leading > 0;
                    let padding = repeat_string(" ", prefix.len())?;
                    let mut body = rope.indent(content.trim_newlines(), &padding)?;
                    if paragraph {
                        let ending = rope.text(&concatenate(&["\n", &padding])?)?;
                        body = rope.concat(body, ending)?;
                    }
                    let prefix = rope.text(&prefix)?;
                    let mut result = rope.concat(prefix, body)?;
                    if node.next.is_some() {
                        result = result.append_newlines(1);
                    }
                    result
                }
                "pre" if name == "PRE" && node.first.is_some_and(|c| self.names[c] == "CODE") => {
                    let text = rope.text(self.text(node.first.unwrap()))?;
                    let text = rope.indent(text, "    ")?;
                    let prefix = rope.text("    ")?;
                    let code = rope.concat(prefix, text)?;
                    wrap_block(rope, code)?
                }
                "hr" => {
                    let rule = rope.text("* * *")?;
                    wrap_block(rope, rule)?
                }
                "a" if name == "A" && self.dom.attr(id, "href").is_some_and(|s| !s.is_empty()) => {
                    let prefix = rope.text("[")?;
                    let suffix = rope.text(&concatenate(&[
                        "](",
                        &destination(self.dom.attr(id, "href").unwrap())?,
                        &title(self.dom.attr(id, "title"))?,
                        ")",
                    ])?)?;
                    let body = rope.concat(prefix, content)?;
                    rope.concat(body, suffix)?
                }
                "em" | "i" | "strong" | "b" => {
                    if rope.blank(content) {
                        Content::default()
                    } else {
                        let marker = if matches!(lower.as_str(), "em" | "i") {
                            "_"
                        } else {
                            "**"
                        };
                        {
                            let marker = rope.text(marker)?;
                            let body = rope.concat(marker, content)?;
                            rope.concat(body, marker)?
                        }
                    }
                }
                "code"
                    if name == "CODE"
                        && !(node.parent.is_some_and(|p| self.names[p] == "PRE")
                            && node.prev.is_none()
                            && node.next.is_none()) =>
                {
                    let text = rope.flatten_and_release(content)?;
                    rope.text_owned(inline_code(text)?)?
                }
                "img" => {
                    let src = self.dom.attr(id, "src").unwrap_or("");
                    if src.is_empty() {
                        Content::default()
                    } else {
                        rope.text(&concatenate(&[
                            "![",
                            &escape(&clean_attribute(self.dom.attr(id, "alt").unwrap_or(""))?)?,
                            "](",
                            &destination(src)?,
                            &title(self.dom.attr(id, "title"))?,
                            ")",
                        ])?)?
                    }
                }
                _ if block => wrap_block(rope, content)?,
                _ => content,
            }
        };
        // Turndown materializes each replacement before join trims boundary
        // newlines. Enforce its limit here even for our symbolic block wraps.
        rope.check(replacement)?;
        if leading.is_empty() && trailing.is_empty() {
            Ok(replacement)
        } else {
            {
                let leading = rope.text(leading)?;
                let trailing = rope.text(trailing)?;
                let body = rope.concat(leading, replacement)?;
                rope.concat(body, trailing)
            }
        }
    }
}

fn escape(text: &str) -> Result<String, String> {
    let escapes = text
        .chars()
        .filter(|c| matches!(c, '\\' | '*' | '`' | '[' | ']' | '_'))
        .count();
    // The optional leading escape is checked separately below.
    check_length(text.encode_utf16().count() + escapes)?;
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if matches!(c, '\\' | '*' | '`' | '[' | ']' | '_') {
            out.push('\\');
        }
        out.push(c);
    }
    let hashes = out.bytes().take_while(|&b| b == b'#').count();
    if out.starts_with(['-', '=', '>'])
        || out.starts_with("+ ")
        || out.starts_with("~~~")
        || (1..=6).contains(&hashes) && out[hashes..].starts_with(' ')
    {
        check_length(out.encode_utf16().count() + 1)?;
        out.insert(0, '\\');
    }
    let digits = out.bytes().take_while(u8::is_ascii_digit).count();
    if digits > 0 && out[digits..].starts_with(". ") {
        check_length(out.encode_utf16().count() + 1)?;
        out.insert(digits, '\\');
    }
    Ok(out)
}

fn clean_attribute(value: &str) -> Result<String, String> {
    check_length(value.encode_utf16().count())?;
    let mut out = String::with_capacity(value.len());
    let mut after_newline = false;
    for c in value.chars() {
        if after_newline && js_space(c) {
            continue;
        }
        out.push(c);
        after_newline = c == '\n';
    }
    Ok(out)
}

fn title(value: Option<&str>) -> Result<String, String> {
    let value = clean_attribute(value.unwrap_or(""))?;
    if value.is_empty() {
        Ok(String::new())
    } else {
        check_length(value.encode_utf16().count() + value.matches('"').count() + 3)?;
        concatenate(&[" \"", &value.replace('"', "\\\""), "\""])
    }
}

fn destination(value: &str) -> Result<String, String> {
    check_length(
        value.encode_utf16().count()
            + value
                .chars()
                .filter(|c| matches!(c, '<' | '>' | '(' | ')'))
                .count()
            + if value.contains(' ') { 2 } else { 0 },
    )?;
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        if matches!(c, '<' | '>' | '(' | ')') {
            out.push('\\');
        }
        out.push(c);
    }
    if out.contains(' ') {
        concatenate(&["<", &out, ">"])
    } else {
        Ok(out)
    }
}

fn inline_code(mut content: String) -> Result<String, String> {
    check_length(content.encode_utf16().count())?;
    if content.is_empty() {
        return Ok(String::new());
    }
    if content.contains(['\r', '\n']) {
        content = content.replace("\r\n", " ").replace(['\r', '\n'], " ");
    }
    let runs: std::collections::HashSet<usize> = content
        .split(|c| c != '`')
        .filter(|s| !s.is_empty())
        .map(str::len)
        .collect();
    let mut count = 1;
    while runs.contains(&count) {
        count += 1;
    }
    let padded = content.starts_with('`')
        || content.ends_with('`')
        || content.starts_with(' ') && content.ends_with(' ') && content.chars().any(|c| c != ' ');
    let pad = if padded { " " } else { "" };
    check_length(content.encode_utf16().count() + 2 * (count + pad.len()))?;
    let delimiter = repeat_string("`", count)?;
    content.reserve(2 * (delimiter.len() + pad.len()));
    content.insert_str(0, pad);
    content.insert_str(0, &delimiter);
    content.push_str(pad);
    content.push_str(&delimiter);
    Ok(content)
}

impl HtmlToMarkdown for Turndown {
    fn convert(&self, html: &str) -> Result<String, String> {
        convert(html, &mut Rope::default())
    }
}

fn convert(html: &str, rope: &mut Rope) -> Result<String, String> {
    if html.is_empty() {
        return Ok(String::new());
    }
    // Match the baseline's document parsing wrapper, including HTML input
    // that closes it early. Parsing a fragment/body changes malformed input.
    let wrapped = concatenate(&["<x-turndown id=\"turndown-root\">", html, "</x-turndown>"])?;
    let mut dom = parse_document(&wrapped)?;
    let root = dom
        .element_by_id("turndown-root")
        .ok_or("HTML root missing")?;
    let names: Vec<_> = (0..dom.nodes.len()).map(|id| dom.node_name(id)).collect();
    collapse(&mut dom, root, &names)?;
    let order: Vec<_> = std::iter::once(root).chain(dom.descendants(root)).collect();
    let render = Render::new(&dom, &names, &order)?;
    let mut output = vec![Content::default(); dom.nodes.len()];
    for &id in order.iter().rev() {
        let value = match &dom.nodes[id].kind {
            Kind::Text(text) if render.summary[id].code => rope.text(text)?,
            Kind::Text(text) => rope.text(&escape(text)?)?,
            Kind::Element { .. } => {
                let mut content = Content::default();
                for child in dom.children(id) {
                    content = rope.join(content, output[child])?;
                }
                if id == root {
                    content
                } else {
                    render.replace(rope, id, content)?
                }
            }
            _ => Content::default(),
        };
        output[id] = value;
    }
    let mut output = rope.flatten(output[root])?;
    let start = output.len() - output.trim_start_matches(['\t', '\r', '\n']).len();
    let end = output.trim_end_matches(js_space).len();
    output.truncate(end.max(start));
    output.drain(..start);
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heading_budget_includes_replacement_newlines_and_utf16_surrogates() {
        // The final Markdown is only 5 UTF-16 units, but Turndown must first
        // materialize the 9-unit replacement "\n\n😀\n==\n\n".
        let html = "<h1>😀</h1>";
        assert_eq!(convert(html, &mut Rope::with_limit(9)).unwrap(), "😀\n==");
        assert_eq!(
            convert(html, &mut Rope::with_limit(8)).unwrap_err(),
            STRING_LIMIT_ERROR
        );
    }

    #[test]
    fn nested_headings_enforce_budget_before_allocating_output() {
        // A bounded negative-control target: removing the budget check yields
        // only 2549 bytes here, rather than attempting the huge n=33 output.
        let depth = 8;
        // Even the mutant's unbounded recurrence fits in 4 KiB. Keep this
        // independent assertion before invoking the converter under test.
        assert!(10usize.checked_shl(depth).unwrap() < 4096);
        let html = format!(
            "{}x{}",
            "<h1><div>".repeat(depth as usize),
            "</div></h1>".repeat(depth as usize)
        );
        let mut rope = Rope::with_limit(1024);
        assert_eq!(convert(&html, &mut rope).unwrap_err(), STRING_LIMIT_ERROR);
        assert!(rope.peak_text_bytes() < 100);
    }

    #[test]
    fn nested_inline_replacements_bound_retained_text() {
        let payload = "x".repeat(256 * 1024);
        let mut failures = Vec::new();
        for tag in ["code", "span"] {
            let render = |depth| {
                let html = format!(
                    "<p>left{}{payload}{}right</p>",
                    format!("<{tag}> ").repeat(depth),
                    format!(" </{tag}>").repeat(depth)
                );
                let mut rope = Rope::default();
                let output = convert(&html, &mut rope).unwrap();
                assert!(output.contains(&payload));
                (rope.peak_text_bytes(), output.len())
            };
            let (single, _) = render(1);
            let (nested, output_bytes) = render(250);
            eprintln!("{tag}: single={single}, nested={nested}, output_bytes={output_bytes}");
            // Counts capacity of every retained Text/Indent string, including
            // old nodes no longer reachable from the final output. No RSS or
            // allocator/platform assumptions; restored flatten must fail.
            if nested > single * 3 {
                failures.push(format!("{tag}: {nested} > 3 * {single}"));
            }
        }
        assert!(failures.is_empty(), "{}", failures.join("; "));
    }
}
