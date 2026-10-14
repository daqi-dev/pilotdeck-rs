// Port of yaml@2.9.0 dist/compose/compose-node.js (ISC).
use super::{
    Context, compose_collection::compose_collection, compose_scalar::compose_scalar,
    resolve_end::resolve_end, resolve_props::Props,
    util_empty_scalar_position::empty_scalar_position,
};
use crate::{
    nodes::{Node, NodeId, NodeKind},
    parse::cst::{Token, TokenType::*},
    string::slice,
};
use pilotdeck_protocol::JsString;
pub(super) fn compose_node(ctx: &mut Context, token: &Token, props: &Props) -> NodeId {
    let id = match token.kind {
        Alias => {
            let id = compose_alias(ctx, token);
            if props.anchor.is_some() || props.tag.is_some() {
                ctx.token_error(
                    token,
                    "ALIAS_PROPS",
                    "An alias node must not specify any properties",
                );
            }
            id
        }
        Scalar | SingleQuotedScalar | DoubleQuotedScalar | BlockScalar => {
            let id = compose_scalar(ctx, token, props.tag.as_ref());
            if let Some(a) = &props.anchor {
                ctx.nodes[id].anchor = Some(slice(
                    a.source.as_ref().unwrap(),
                    1,
                    a.source.as_ref().unwrap().len_utf16(),
                ));
            }
            id
        }
        BlockMap | BlockSeq | FlowCollection => {
            let id = compose_collection(ctx, token, props);
            if let Some(a) = &props.anchor {
                ctx.nodes[id].anchor = Some(slice(
                    a.source.as_ref().unwrap(),
                    1,
                    a.source.as_ref().unwrap().len_utf16(),
                ));
            }
            id
        }
        _ => {
            ctx.token_error(
                token,
                "UNEXPECTED_TOKEN",
                token.message.clone().unwrap_or_else(|| {
                    format!("Unsupported token (type: {})", token.kind.as_str()).into()
                }),
            );
            compose_empty_node(ctx, token.offset, &[], props)
        }
    };
    if let Some(a) = &props.anchor
        && ctx.nodes[id]
            .anchor
            .as_ref()
            .is_some_and(JsString::is_empty)
    {
        ctx.token_error(a, "BAD_ALIAS", "Anchor cannot be an empty string");
    }
    let node = &mut ctx.nodes[id];
    node.space_before = props.space_before;
    if !props.comment.is_empty() {
        if token.kind == Scalar && token.source.as_ref().is_some_and(JsString::is_empty) {
            node.comment = props.comment.clone();
        } else {
            node.comment_before = props.comment.clone();
        }
    }
    if ctx.options.keep_source_tokens && !matches!(token.kind, Error) {
        node.src_token = Some(token.clone());
    }
    id
}
pub(super) fn compose_empty_node(
    ctx: &mut Context,
    offset: usize,
    before: &[Token],
    props: &Props,
) -> NodeId {
    let mut token = Token::new(Scalar, empty_scalar_position(offset, before));
    token.source = Some(JsString::default());
    let id = compose_scalar(ctx, &token, props.tag.as_ref());
    if let Some(a) = &props.anchor {
        let anchor = slice(
            a.source.as_ref().unwrap(),
            1,
            a.source.as_ref().unwrap().len_utf16(),
        );
        if anchor.is_empty() {
            ctx.token_error(a, "BAD_ALIAS", "Anchor cannot be an empty string");
        }
        ctx.nodes[id].anchor = Some(anchor);
    }
    let node = &mut ctx.nodes[id];
    node.space_before = props.space_before;
    if !props.comment.is_empty() {
        node.comment = props.comment.clone();
        node.range[2] = props.end;
    }
    id
}
fn compose_alias(ctx: &mut Context, token: &Token) -> NodeId {
    let source = token.source.as_ref().unwrap();
    let name = slice(source, 1, source.len_utf16());
    if name.is_empty() {
        ctx.at(token.offset, "BAD_ALIAS", "Alias cannot be an empty string");
    }
    if name.units().last() == Some(&58) {
        let pos = token.offset + source.len_utf16() - 1;
        ctx.warn(
            [pos, pos + 1],
            "BAD_ALIAS",
            "Alias ending in : is ambiguous",
        );
    }
    let value_end = token.offset + source.len_utf16();
    let re = resolve_end(
        ctx,
        token.end.as_deref().unwrap_or_default(),
        value_end,
        true,
    );
    let mut alias = Node::new(NodeKind::Alias(name));
    alias.range = [token.offset, value_end, re.offset];
    alias.comment = re.comment;
    ctx.add(alias)
}
