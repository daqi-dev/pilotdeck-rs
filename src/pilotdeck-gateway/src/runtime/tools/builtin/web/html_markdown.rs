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
    metadata::{js_space, js_trim},
};
use std::ops::Range;

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

fn collapse_text(text: &str) -> String {
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
    out
}

/// Visit block boundaries on both entry and exit, leaving PRE subtrees intact.
fn collapse(dom: &mut Dom, root: Id, names: &[String]) {
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
            let mut text = collapse_text(text);
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
    fn new(dom: &'a Dom, names: &'a [String], order: &[Id]) -> Self {
        let mut summary: Vec<Summary> = (0..dom.nodes.len()).map(|_| Summary::default()).collect();
        let mut text = String::new();
        for &id in order {
            summary[id].code =
                names[id] == "CODE" || dom.nodes[id].parent.is_some_and(|p| summary[p].code);
            if let Kind::Text(value) = &dom.nodes[id].kind {
                let start = text.len();
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
        Self {
            dom,
            names,
            summary,
            text,
        }
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

    fn replace(&self, id: Id, content: String) -> String {
        let name = self.names[id].as_str();
        let lower = name.to_ascii_lowercase();
        let node = &self.dom.nodes[id];
        let block = BLOCK.contains(&name);
        let (leading, trailing) = self.flanks(id);
        let mut content = content;
        if !leading.is_empty() || !trailing.is_empty() {
            content = js_trim(&content).to_owned();
        }
        let wrap_block = |content: &str| format!("\n\n{content}\n\n");
        // Domino's getElementsByTagName("IMG"/"A"/...) folds case only on
        // HTML elements. Foreign ancestors can therefore still be blank even
        // when their HTML integration-point descendants contain an image/link.
        let meaningful = VOID.contains(&name)
            || MEANINGFUL.contains(&name)
            || self.dom.is_html(id) && self.summary[id].meaningful;
        let replacement = if !self.summary[id].nonblank && !meaningful {
            if block {
                "\n\n".to_string()
            } else {
                String::new()
            }
        } else {
            match lower.as_str() {
                "p" => wrap_block(&content),
                "br" => "  \n".into(),
                "h1" | "h2" => {
                    let marker = if lower == "h1" { "=" } else { "-" };
                    wrap_block(&format!(
                        "{content}\n{}",
                        marker.repeat(content.encode_utf16().count())
                    ))
                }
                "h3" | "h4" | "h5" | "h6" => wrap_block(&format!(
                    "{} {content}",
                    "#".repeat((name.as_bytes()[1] - b'0') as usize)
                )),
                "blockquote" => {
                    let content = content.trim_matches('\n').replace('\n', "\n> ");
                    wrap_block(&format!("> {content}"))
                }
                "ul" | "ol" => {
                    let nested_last = node.parent.is_some_and(|p| {
                        self.names[p] == "LI" && self.summary[p].last_element == Some(id)
                    });
                    if nested_last {
                        format!("\n{content}")
                    } else {
                        wrap_block(&content)
                    }
                }
                "li" => {
                    let prefix =
                        if let Some(parent) = node.parent.filter(|&p| self.names[p] == "OL") {
                            let start = self.dom.attr(parent, "start").filter(|s| !s.is_empty());
                            let n = start.map_or(1.0, string_to_number)
                                + self.summary[id].ordinal as f64;
                            format!("{}.  ", number_string(n))
                        } else {
                            "*   ".to_string()
                        };
                    let paragraph = if content.ends_with('\n') { "\n" } else { "" };
                    let content = format!("{}{paragraph}", content.trim_matches('\n'))
                        .replace('\n', &format!("\n{}", " ".repeat(prefix.len())));
                    format!(
                        "{prefix}{content}{}",
                        if node.next.is_some() { "\n" } else { "" }
                    )
                }
                "pre" if name == "PRE" && node.first.is_some_and(|c| self.names[c] == "CODE") => {
                    wrap_block(&format!(
                        "    {}",
                        self.text(node.first.unwrap()).replace('\n', "\n    ")
                    ))
                }
                "hr" => wrap_block("* * *"),
                "a" if name == "A" && self.dom.attr(id, "href").is_some_and(|s| !s.is_empty()) => {
                    format!(
                        "[{content}]({}{})",
                        destination(self.dom.attr(id, "href").unwrap()),
                        title(self.dom.attr(id, "title"))
                    )
                }
                "em" | "i" | "strong" | "b" => {
                    if js_trim(&content).is_empty() {
                        String::new()
                    } else {
                        let marker = if matches!(lower.as_str(), "em" | "i") {
                            "_"
                        } else {
                            "**"
                        };
                        format!("{marker}{content}{marker}")
                    }
                }
                "code"
                    if name == "CODE"
                        && !(node.parent.is_some_and(|p| self.names[p] == "PRE")
                            && node.prev.is_none()
                            && node.next.is_none()) =>
                {
                    inline_code(&content)
                }
                "img" => {
                    let src = self.dom.attr(id, "src").unwrap_or("");
                    if src.is_empty() {
                        String::new()
                    } else {
                        format!(
                            "![{}]({}{})",
                            escape(&clean_attribute(self.dom.attr(id, "alt").unwrap_or(""))),
                            destination(src),
                            title(self.dom.attr(id, "title"))
                        )
                    }
                }
                _ if block => wrap_block(&content),
                _ => content,
            }
        };
        if leading.is_empty() && trailing.is_empty() {
            replacement
        } else {
            format!("{leading}{replacement}{trailing}")
        }
    }
}

fn join(output: &mut String, mut replacement: String) {
    let trailing = output.len() - output.trim_end_matches('\n').len();
    let leading = replacement.len() - replacement.trim_start_matches('\n').len();
    if output.is_empty() {
        // Deep chains of identity elements transfer their child's buffer
        // instead of copying a large payload once for every ancestor.
        replacement.drain(..leading.saturating_sub(2));
        *output = replacement;
        return;
    }
    output.truncate(output.len() - trailing);
    for _ in 0..trailing.max(leading).min(2) {
        output.push('\n');
    }
    output.push_str(&replacement[leading..]);
}

fn escape(text: &str) -> String {
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
        out.insert(0, '\\');
    }
    let digits = out.bytes().take_while(u8::is_ascii_digit).count();
    if digits > 0 && out[digits..].starts_with(". ") {
        out.insert(digits, '\\');
    }
    out
}

fn clean_attribute(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut after_newline = false;
    for c in value.chars() {
        if after_newline && js_space(c) {
            continue;
        }
        out.push(c);
        after_newline = c == '\n';
    }
    out
}

fn title(value: Option<&str>) -> String {
    let value = clean_attribute(value.unwrap_or(""));
    if value.is_empty() {
        String::new()
    } else {
        format!(" \"{}\"", value.replace('"', "\\\""))
    }
}

fn destination(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        if matches!(c, '<' | '>' | '(' | ')') {
            out.push('\\');
        }
        out.push(c);
    }
    if out.contains(' ') {
        format!("<{out}>")
    } else {
        out
    }
}

fn inline_code(content: &str) -> String {
    if content.is_empty() {
        return String::new();
    }
    let content = content.replace("\r\n", " ").replace(['\r', '\n'], " ");
    let runs: std::collections::HashSet<usize> = content
        .split(|c| c != '`')
        .filter(|s| !s.is_empty())
        .map(str::len)
        .collect();
    let mut count = 1;
    while runs.contains(&count) {
        count += 1;
    }
    let delimiter = "`".repeat(count);
    let padded = content.starts_with('`')
        || content.ends_with('`')
        || content.starts_with(' ') && content.ends_with(' ') && content.chars().any(|c| c != ' ');
    let pad = if padded { " " } else { "" };
    format!("{delimiter}{pad}{content}{pad}{delimiter}")
}

impl HtmlToMarkdown for Turndown {
    fn convert(&self, html: &str) -> Result<String, String> {
        if html.is_empty() {
            return Ok(String::new());
        }
        // Match the baseline's document parsing wrapper, including HTML input
        // that closes it early. Parsing a fragment/body changes malformed input.
        let mut dom = parse_document(&format!(
            "<x-turndown id=\"turndown-root\">{html}</x-turndown>"
        ));
        let root = dom
            .element_by_id("turndown-root")
            .ok_or("HTML root missing")?;
        let names: Vec<_> = (0..dom.nodes.len()).map(|id| dom.node_name(id)).collect();
        collapse(&mut dom, root, &names);
        let order: Vec<_> = std::iter::once(root).chain(dom.descendants(root)).collect();
        let render = Render::new(&dom, &names, &order);
        let mut output: Vec<String> = (0..dom.nodes.len()).map(|_| String::new()).collect();
        for &id in order.iter().rev() {
            let value = match &dom.nodes[id].kind {
                Kind::Text(text) if render.summary[id].code => text.clone(),
                Kind::Text(text) => escape(text),
                Kind::Element { .. } => {
                    let mut content = String::new();
                    for child in dom.children(id) {
                        join(&mut content, std::mem::take(&mut output[child]));
                    }
                    if id == root {
                        content
                    } else {
                        render.replace(id, content)
                    }
                }
                _ => String::new(),
            };
            output[id] = value;
        }
        Ok(output[root]
            .trim_start_matches(['\t', '\r', '\n'])
            .trim_end_matches(js_space)
            .to_owned())
    }
}
