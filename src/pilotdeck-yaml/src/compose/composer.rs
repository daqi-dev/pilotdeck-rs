// Port of yaml@2.9.0 dist/compose/composer.js (ISC).
use super::{Context, compose_doc::compose_doc, resolve_end::resolve_end};
use crate::{
    Options,
    doc::{Document, directives::Directives},
    errors::{Diagnostic, token_pos},
    parse::cst::{Token, TokenType::*},
    schema::Schema,
    string::{append, concat, json, slice},
};
use pilotdeck_protocol::JsString;
fn parse_prelude(prelude: &[JsString]) -> (JsString, bool) {
    let mut comment = JsString::default();
    let mut at_comment = false;
    let mut after_empty_line = false;
    let mut i = 0;
    while i < prelude.len() {
        let source = &prelude[i];
        match source.units().first() {
            Some(35) => {
                if !comment.is_empty() {
                    append(
                        &mut comment,
                        &if after_empty_line { "\n\n" } else { "\n" }.into(),
                    );
                }
                let c = if source.len_utf16() > 1 {
                    slice(source, 1, source.len_utf16())
                } else {
                    " ".into()
                };
                append(&mut comment, &c);
                at_comment = true;
                after_empty_line = false;
            }
            Some(37) => {
                if prelude
                    .get(i + 1)
                    .is_none_or(|s| s.units().first() != Some(&35))
                {
                    i += 1;
                }
                at_comment = false;
            }
            _ => {
                if !at_comment {
                    after_empty_line = true;
                }
                at_comment = false;
            }
        }
        i += 1;
    }
    (comment, after_empty_line)
}
pub(crate) struct Composer {
    doc: Option<Document>,
    at_directives: bool,
    prelude: Vec<JsString>,
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
            prelude: Vec::new(),
            errors: Vec::new(),
            warnings: Vec::new(),
            directives: Directives::default(),
            options: crate::ParseOptions {
                keep_source_tokens: options.keep_source_tokens,
            },
            output: Vec::new(),
        }
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
            nodes: Vec::new(),
            errors: Vec::new(),
            warnings: Vec::new(),
        }
    }
    fn decorate(&mut self, doc: &mut Document, after_doc: bool) {
        let (comment, after_empty_line) = parse_prelude(&self.prelude);
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
        self.prelude.clear();
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
            comment: parse_prelude(&self.prelude).0,
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
                let mut doc = compose_doc(ctx, &token);
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
                    nodes: Vec::new(),
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
                nodes: Vec::new(),
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
