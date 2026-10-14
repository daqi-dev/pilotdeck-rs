// Port of yaml@2.9.0 dist/compose/resolve-props.js (ISC).
use super::Context;
use crate::{
    errors::token_pos,
    parse::cst::{Token, TokenType, TokenType::*},
    string::concat,
};
use pilotdeck_protocol::JsString;
#[derive(Clone, Debug, Default)]
pub(super) struct Props {
    pub comma: Option<Token>,
    pub found: Option<Token>,
    pub space_before: bool,
    pub comment: JsString,
    pub has_newline: bool,
    pub anchor: Option<Token>,
    pub tag: Option<Token>,
    pub newline_after_prop: Option<Token>,
    pub end: usize,
    pub start: usize,
}
pub(super) struct PropOptions<'a> {
    pub flow: Option<&'static str>,
    pub indicator: TokenType,
    pub next: Option<&'a Token>,
    pub offset: usize,
    pub parent_indent: usize,
    pub start_on_newline: bool,
}
pub(super) fn resolve_props(ctx: &mut Context, tokens: &[Token], opts: PropOptions<'_>) -> Props {
    let PropOptions {
        flow,
        indicator,
        next,
        offset,
        parent_indent,
        start_on_newline,
    } = opts;
    let mut p = Props::default();
    let mut at_newline = start_on_newline;
    let mut has_space = start_on_newline;
    let mut comment = Vec::new();
    let mut comment_sep = Vec::new();
    let mut req_space = false;
    let mut tab: Option<&Token> = None;
    let mut start = None;
    for token in tokens {
        if req_space {
            if !matches!(token.kind, Space | Newline | Comma) {
                ctx.at(
                    token.offset,
                    "MISSING_CHAR",
                    "Tags and anchors must be separated from the next token by white space",
                );
            }
            req_space = false;
        }
        if let Some(t) = tab.take()
            && at_newline
            && !matches!(token.kind, Comment | Newline)
        {
            ctx.token_error(t, "TAB_AS_INDENT", "Tabs are not allowed as indentation");
        }
        let source = token.source.as_ref().unwrap();
        match token.kind {
            Space => {
                if flow.is_none()
                    && (indicator != DocStart || next.is_none_or(|t| t.kind != FlowCollection))
                    && source.units().contains(&9)
                {
                    tab = Some(token);
                }
                has_space = true;
            }
            Comment => {
                if !has_space {
                    ctx.token_error(
                        token,
                        "MISSING_CHAR",
                        "Comments must be separated from other tokens by white space characters",
                    );
                }
                if !comment.is_empty() {
                    comment.extend_from_slice(&comment_sep);
                }
                if source.len_utf16() > 1 {
                    comment.extend_from_slice(&source.units()[1..]);
                } else {
                    comment.push(32);
                }
                comment_sep.clear();
                at_newline = false;
            }
            Newline => {
                if at_newline {
                    if !comment.is_empty() {
                        comment.extend_from_slice(source.units());
                    } else if p.found.is_none() || indicator != SeqItemInd {
                        p.space_before = true;
                    }
                } else {
                    comment_sep.extend_from_slice(source.units());
                }
                at_newline = true;
                p.has_newline = true;
                if p.anchor.is_some() || p.tag.is_some() {
                    p.newline_after_prop = Some(token.clone());
                }
                has_space = true;
            }
            Anchor => {
                if p.anchor.is_some() {
                    ctx.token_error(
                        token,
                        "MULTIPLE_ANCHORS",
                        "A node can have at most one anchor",
                    );
                }
                if source.units().last() == Some(&58) {
                    let pos = token.offset + source.len_utf16() - 1;
                    ctx.warn(
                        [pos, pos + 1],
                        "BAD_ALIAS",
                        "Anchor ending in : is ambiguous",
                    );
                }
                p.anchor = Some(token.clone());
                start.get_or_insert(token.offset);
                at_newline = false;
                has_space = false;
                req_space = true;
            }
            Tag => {
                if p.tag.is_some() {
                    ctx.token_error(token, "MULTIPLE_TAGS", "A node can have at most one tag");
                }
                p.tag = Some(token.clone());
                start.get_or_insert(token.offset);
                at_newline = false;
                has_space = false;
                req_space = true;
            }
            kind if kind == indicator => {
                if p.anchor.is_some() || p.tag.is_some() {
                    ctx.token_error(
                        token,
                        "BAD_PROP_ORDER",
                        concat(&[
                            &"Anchors and tags must be after the ".into(),
                            source,
                            &" indicator".into(),
                        ]),
                    );
                }
                if p.found.is_some() {
                    ctx.token_error(
                        token,
                        "UNEXPECTED_TOKEN",
                        concat(&[
                            &"Unexpected ".into(),
                            source,
                            &format!(" in {}", flow.unwrap_or("collection")).into(),
                        ]),
                    );
                }
                p.found = Some(token.clone());
                at_newline = matches!(indicator, SeqItemInd | ExplicitKeyInd);
                has_space = false;
            }
            Comma if flow.is_some() => {
                if p.comma.is_some() {
                    ctx.token_error(
                        token,
                        "UNEXPECTED_TOKEN",
                        format!("Unexpected , in {}", flow.unwrap()),
                    );
                }
                p.comma = Some(token.clone());
                at_newline = false;
                has_space = false;
            }
            _ => {
                ctx.token_error(
                    token,
                    "UNEXPECTED_TOKEN",
                    format!("Unexpected {} token", token.kind.as_str()),
                );
                at_newline = false;
                has_space = false;
            }
        }
    }
    p.end = tokens.last().map_or(offset, |t| token_pos(t)[1]);
    if req_space
        && let Some(next) = next
        && !matches!(next.kind, Space | Newline | Comma)
        && (next.kind != Scalar || next.source.as_ref().is_some_and(|s| !s.is_empty()))
    {
        ctx.at(
            next.offset,
            "MISSING_CHAR",
            "Tags and anchors must be separated from the next token by white space",
        );
    }
    if let Some(tab) = tab
        && ((at_newline && tab.indent.unwrap_or(0) <= parent_indent)
            || next.is_some_and(|t| matches!(t.kind, BlockMap | BlockSeq)))
    {
        ctx.token_error(tab, "TAB_AS_INDENT", "Tabs are not allowed as indentation");
    }
    p.comment = JsString::from_utf16(comment);
    p.start = start.unwrap_or(p.end);
    p
}
