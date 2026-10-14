// Port of yaml@2.9.0 dist/public-api.js (ISC), parse entry points.
//! Faithful, UTF-16 port of the parsing pipeline of npm yaml 2.9.0.
//! See README.md for the currently completed stages and API boundaries.
mod compose;
pub mod doc;
pub mod errors;
pub mod nodes;
pub mod parse;
mod schema;
mod stack;
mod string;
pub mod value;
pub use doc::{Document, Documents, StreamInfo};
use pilotdeck_protocol::JsString;
#[derive(Clone, Debug)]
pub struct Options {
    pub pretty_errors: bool,
    pub keep_source_tokens: bool,
    pub line_counter: Option<std::rc::Rc<std::cell::RefCell<parse::line_counter::LineCounter>>>,
}
impl Default for Options {
    fn default() -> Self {
        Self {
            pretty_errors: true,
            keep_source_tokens: false,
            line_counter: None,
        }
    }
}
// Only composition's supported option is stored in nodes/contexts. The caller's
// Rc LineCounter stays on the calling thread; line updates are applied afterward.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ParseOptions {
    pub keep_source_tokens: bool,
}
/// Parse with the baseline's default schema/strictness and UTF-16 positions.
pub fn parse_document(src: &JsString, options: Options) -> Document {
    let mut parser = parse::parser::Parser::default();
    // public-api.js stops the parser/composer generators on the second yielded
    // document. In particular, a supplied LineCounter must not consume the tail.
    parser.line_counter.add_new_line(0);
    let mut composer = compose::composer::Composer::new(options.clone());
    let mut documents = Vec::new();
    'parse: for lexeme in parse::lexer::Lexer::default().lex(src, false) {
        for token in parser.next(lexeme) {
            composer.next(token);
            documents.extend(composer.take_documents());
            if documents.len() >= 2 {
                break 'parse;
            }
        }
    }
    if documents.len() < 2 {
        for token in parser.end() {
            composer.next(token);
            documents.extend(composer.take_documents());
            if documents.len() >= 2 {
                break;
            }
        }
        if documents.len() < 2 {
            composer.end(true, src.len_utf16());
            documents.extend(composer.take_documents());
        }
    }
    let mut docs = documents.into_iter();
    let mut doc = docs.next().unwrap();
    if let Some(next) = docs.next() {
        doc.errors.push(errors::Diagnostic::new(
            [next.range[0], next.range[1]],
            "MULTIPLE_DOCS",
            "Source contains multiple documents; please use YAML.parseAllDocuments()",
            false,
        ));
    }
    doc.line_counter = std::sync::Arc::new(collect_lines(&options, parser.line_counter));
    if options.pretty_errors {
        for error in doc.errors.iter_mut().chain(doc.warnings.iter_mut()) {
            errors::prettify_error(src, &doc.line_counter, error);
        }
    }
    doc
}
pub fn parse_all_documents(src: &JsString, options: Options) -> Documents {
    let mut parser = parse::parser::Parser::default();
    let mut composer = compose::composer::Composer::new(options.clone());
    let mut separators = 0;
    let mut lines = 1;
    let mut prelude_capacity = 0;
    let mut in_prelude = true;
    for line in src.units().split(|c| *c == 10) {
        lines += 1;
        if line.starts_with(&[45, 45, 45]) && line.get(3).is_none_or(|c| matches!(c, 9 | 13 | 32)) {
            separators += 1;
        }
        if in_prelude {
            if line.first() == Some(&35) {
                prelude_capacity += line.len().saturating_sub(1).max(1) + 1;
            } else if !line.iter().all(|c| matches!(c, 9 | 32)) {
                in_prelude = false;
            }
        }
    }
    composer.reserve_documents(separators + 1);
    composer.reserve_prelude(prelude_capacity);
    if in_prelude && options.line_counter.is_none() {
        // A stream consisting only of comments/blank lines exposes no Document
        // line table. A supplied counter still receives the normal parser lines.
        parser.defer_lines();
    } else {
        parser.line_counter.add_new_line(0);
        parser.line_counter.line_starts.reserve_exact(lines);
    }

    // Compose each yielded token immediately; retaining the whole CST stream
    // alongside every output document needlessly doubles peak memory.
    parse::lexer::Lexer::lex_complete_with(src, |lexeme| {
        for token in parser.next(lexeme) {
            composer.next(token);
        }
    });
    for token in parser.end() {
        composer.next(token);
    }
    let (mut docs, info) = composer.compose(Vec::new(), false, src.len_utf16());
    let lines = std::sync::Arc::new(collect_lines(&options, parser.line_counter));
    for doc in &mut docs {
        doc.line_counter = lines.clone();
        if options.pretty_errors {
            for error in doc.errors.iter_mut().chain(doc.warnings.iter_mut()) {
                errors::prettify_error(src, &doc.line_counter, error);
            }
        }
    }
    let stream_info = docs.is_empty().then_some(info);
    Documents {
        documents: docs,
        stream_info,
    }
}
/// A dynamically supplied option outside the baseline-supported subset.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnsupportedOption {
    pub name: JsString,
    pub message: JsString,
}
impl Options {
    /// Accept only options used by the pinned call sites. Source line positions
    /// are always collected and exposed on Document::line_counter.
    pub fn from_js(options: &pilotdeck_protocol::JsObject) -> Result<Self, UnsupportedOption> {
        let mut out = Self::default();
        for (name, value) in options.entries() {
            let target = if name.equals_str("prettyErrors") {
                &mut out.pretty_errors
            } else if name.equals_str("keepSourceTokens") {
                &mut out.keep_source_tokens
            } else {
                return Err(UnsupportedOption {
                    name: name.clone(),
                    message: string::message("Unsupported YAML option: ", name),
                });
            };
            let Some(value) = value.as_bool() else {
                return Err(UnsupportedOption {
                    name: name.clone(),
                    message: string::message("YAML option requires a boolean: ", name),
                });
            };
            *target = value;
        }
        Ok(out)
    }
}

fn collect_lines(
    options: &Options,
    lines: parse::line_counter::LineCounter,
) -> parse::line_counter::LineCounter {
    if let Some(counter) = &options.line_counter {
        let mut counter = counter.borrow_mut();
        counter.line_starts.extend(lines.line_starts);
        counter.clone()
    } else {
        lines
    }
}
