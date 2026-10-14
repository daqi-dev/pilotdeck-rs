// Port of yaml@2.9.0 dist/compose/composer.js (ISC).
use super::{Context, compose_doc::compose_doc, resolve_end::resolve_end};
use crate::{
    Options,
    doc::{Document, directives::Directives},
    errors::{Diagnostic, token_pos},
    parse::cst::{Token, TokenType::*},
    schema::Schema,
    string::{append, concat, json},
};
use pilotdeck_protocol::JsString;
#[derive(Default)]
struct Prelude {
    comment: Vec<u16>,
    at_comment: bool,
    after_empty_line: bool,
    after_directive: bool,
}
impl Prelude {
    fn push(&mut self, source: std::sync::Arc<JsString>) {
        // parsePrelude skips the token after a directive unless it is a comment.
        if std::mem::take(&mut self.after_directive) && source.units().first() != Some(&35) {
            return;
        }
        match source.units().first() {
            Some(35) => {
                if !self.comment.is_empty() {
                    self.comment.push(10);
                    if self.after_empty_line {
                        self.comment.push(10);
                    }
                }
                if source.len_utf16() > 1 {
                    self.comment.extend_from_slice(&source.units()[1..]);
                } else {
                    self.comment.push(32);
                }
                self.at_comment = true;
                self.after_empty_line = false;
            }
            Some(37) => {
                self.after_directive = true;
                self.at_comment = false;
            }
            _ => {
                if !self.at_comment {
                    self.after_empty_line = true;
                }
                self.at_comment = false;
            }
        }
    }
    fn take(&mut self) -> (JsString, bool) {
        let prelude = std::mem::take(self);
        (
            JsString::from_utf16(prelude.comment),
            prelude.after_empty_line,
        )
    }
}
pub(crate) struct Composer {
    doc: Option<Document>,
    at_directives: bool,
    prelude: Prelude,
    errors: Vec<Diagnostic>,
    warnings: Vec<Diagnostic>,
    directives: Directives,
    options: crate::ParseOptions,
    output: Vec<Document>,
}
impl Composer {
    pub fn new(options: Options) -> Self {
        Self {
            doc: None,
            at_directives: false,
            prelude: Prelude::default(),
            errors: Vec::new(),
            warnings: Vec::new(),
            directives: Directives::default(),
            options: crate::ParseOptions {
                keep_source_tokens: options.keep_source_tokens,
            },
            output: Vec::new(),
        }
    }
    pub(crate) fn reserve_prelude(&mut self, count: usize) {
        self.prelude.comment.reserve_exact(count);
    }
    pub(crate) fn reserve_documents(&mut self, count: usize) {
        self.output.reserve_exact(count);
    }
    pub(crate) fn context(&mut self) -> Context {
        let mut directives = self.directives.at_document();
        // Document constructor chooses its default version unless %YAML is explicit.
        if !directives.explicit {
            directives.version = "1.2";
        }
        let schema = Schema::new(directives.version == "1.1");
        Context {
            depth: 0,
            depth_limit: crate::stack::FLOW_SEQ,
            at_key: false,
            at_root: true,
            directives,
            schema,
            options: self.options,
            nodes: Vec::with_capacity(1),
            errors: Vec::new(),
            warnings: Vec::new(),
        }
    }
    fn decorate(&mut self, doc: &mut Document, after_doc: bool) {
        let (comment, after_empty_line) = self.prelude.take();
        if !comment.is_empty() {
            if after_doc {
                if !doc.comment.is_empty() {
                    append(&mut doc.comment, &"\n".into());
                }
                append(&mut doc.comment, &comment);
            } else if after_empty_line || doc.directives.doc_start || doc.contents.is_none() {
                doc.comment_before = comment;
            } else {
                let root = doc.contents.unwrap();
                let target = if !doc.nodes[root].flow {
                    match &doc.nodes[root].kind {
                        crate::nodes::NodeKind::Map(items) if !items.is_empty() => items[0].key,
                        crate::nodes::NodeKind::Seq(items) if !items.is_empty() => items[0],
                        _ => root,
                    }
                } else {
                    root
                };
                let node = &mut doc.nodes[target];
                node.comment_before = if node.comment_before.is_empty() {
                    comment
                } else {
                    concat(&[&comment, &"\n".into(), &node.comment_before])
                };
            }
        }
        if after_doc {
            doc.errors.append(&mut self.errors);
            doc.warnings.append(&mut self.warnings);
        } else {
            self.errors.append(&mut doc.errors);
            self.warnings.append(&mut doc.warnings);
            doc.errors = std::mem::take(&mut self.errors);
            doc.warnings = std::mem::take(&mut self.warnings);
        }
    }
    pub fn compose(
        mut self,
        tokens: Vec<Token>,
        force_doc: bool,
        end_offset: usize,
    ) -> (Vec<Document>, crate::doc::StreamInfo) {
        for token in tokens {
            self.next(token);
        }
        self.end(force_doc, end_offset);
        let info = crate::doc::StreamInfo {
            comment: self.prelude.take().0,
            directives: self.directives,
            errors: self.errors,
            warnings: self.warnings,
        };
        (self.output, info)
    }
    pub(crate) fn take_documents(&mut self) -> Vec<Document> {
        std::mem::take(&mut self.output)
    }
    pub(crate) fn next(&mut self, mut token: Token) {
        match token.kind {
            Directive => {
                for (offset, msg, warning) in self.directives.add(token.source.as_ref().unwrap()) {
                    let mut pos = token_pos(&token);
                    pos[0] += offset;
                    let d = Diagnostic::new(pos, "BAD_DIRECTIVE", msg, warning);
                    if warning {
                        self.warnings.push(d);
                    } else {
                        self.errors.push(d);
                    }
                }
                self.prelude.push(token.source.take().unwrap());
                self.at_directives = true;
            }
            Document => {
                let ctx = self.context();
                let mut doc = compose_doc(ctx, &mut token);
                // onError is shared upstream: directive errors precede compose errors,
                // while missing doc-start is reported after composition.
                if self.at_directives && !doc.directives.doc_start {
                    doc.errors.push(Diagnostic::new(
                        token_pos(&token),
                        "MISSING_CHAR",
                        "Missing directives-end/doc-start indicator line",
                        false,
                    ));
                }
                self.decorate(&mut doc, false);
                if let Some(prev) = self.doc.replace(doc) {
                    self.output.push(prev);
                }
                self.at_directives = false;
            }
            ByteOrderMark | Space => (),
            Comment | Newline => self.prelude.push(token.source.take().unwrap()),
            Error => {
                let source = token.source.as_ref().unwrap();
                let msg = if source.is_empty() {
                    token.message.clone().unwrap()
                } else {
                    concat(&[token.message.as_ref().unwrap(), &": ".into(), &json(source)])
                };
                let error = Diagnostic::new(token_pos(&token), "UNEXPECTED_TOKEN", msg, false);
                if let Some(doc) = self.doc.as_mut().filter(|_| !self.at_directives) {
                    doc.errors.push(error);
                } else {
                    self.errors.push(error);
                }
            }
            DocEnd => {
                let Some(mut doc) = self.doc.take() else {
                    self.errors.push(Diagnostic::new(
                        token_pos(&token),
                        "UNEXPECTED_TOKEN",
                        "Unexpected doc-end without preceding document",
                        false,
                    ));
                    return;
                };
                doc.directives.doc_end = true;
                let mut ctx = Context {
                    depth: 0,
                    depth_limit: crate::stack::FLOW_SEQ,
                    at_key: false,
                    at_root: false,
                    directives: doc.directives.clone(),
                    schema: doc.schema.clone(),
                    options: doc.options,
                    nodes: Vec::with_capacity(1),
                    errors: Vec::new(),
                    warnings: Vec::new(),
                };
                let end = resolve_end(
                    &mut ctx,
                    token.end.as_deref().unwrap_or_default(),
                    token_pos(&token)[1],
                    true,
                );
                self.errors.extend(ctx.errors);
                self.warnings.extend(ctx.warnings);
                self.decorate(&mut doc, true);
                if !end.comment.is_empty() {
                    if !doc.comment.is_empty() {
                        append(&mut doc.comment, &"\n".into());
                    }
                    append(&mut doc.comment, &end.comment);
                }
                doc.range[2] = end.offset;
                self.doc = Some(doc);
            }
            _ => self.errors.push(Diagnostic::new(
                token_pos(&token),
                "UNEXPECTED_TOKEN",
                format!("Unsupported token {}", token.kind.as_str()),
                false,
            )),
        }
    }
    pub(crate) fn end(&mut self, force_doc: bool, end_offset: usize) {
        if let Some(mut doc) = self.doc.take() {
            self.decorate(&mut doc, true);
            self.output.push(doc);
        } else if force_doc {
            let ctx = self.context();
            let mut doc = Document {
                contents: None,
                nodes: Vec::with_capacity(1),
                directives: ctx.directives,
                schema: ctx.schema,
                options: ctx.options,
                errors: Vec::new(),
                warnings: Vec::new(),
                range: [0, end_offset, end_offset],
                comment: Default::default(),
                comment_before: Default::default(),
                line_counter: Default::default(),
            };
            if self.at_directives {
                self.errors.push(Diagnostic::new(
                    [end_offset, end_offset + 1],
                    "MISSING_CHAR",
                    "Missing directives-end indicator line",
                    false,
                ));
            }
            self.decorate(&mut doc, false);
            self.output.push(doc);
        }
    }
}
