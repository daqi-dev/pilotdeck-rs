// Port of yaml@2.9.0 dist/compose/compose-doc.js (ISC).
use super::{
    Context,
    compose_node::{compose_empty_node, compose_owned_node},
    resolve_end::resolve_end,
    resolve_props::{PropOptions, resolve_props},
};
use crate::{
    doc::Document,
    parse::cst::{Token, TokenType::*},
};
pub(super) fn compose_doc(ctx: Context, token: &mut Token) -> Document {
    crate::stack::with_large_stack(token.nesting_depth_exceeds(64), || {
        compose_doc_inner(ctx, token)
    })
}
fn compose_doc_inner(mut ctx: Context, token: &mut Token) -> Document {
    ctx.depth_limit = crate::stack::compose_limit(token);
    ctx.nodes
        .reserve_exact(token.value.as_deref().map_or(1, Token::node_capacity));
    let mut value = token.value.take();
    let end = token.end.as_deref().unwrap_or_default();
    let props = resolve_props(
        &mut ctx,
        &token.start,
        PropOptions {
            flow: None,
            indicator: DocStart,
            next: value.as_deref().or_else(|| end.first()),
            offset: token.offset,
            parent_indent: 0,
            start_on_newline: true,
        },
    );
    if props.found.is_some() {
        ctx.directives.doc_start = true;
        if value
            .as_deref()
            .is_some_and(|v| matches!(v.kind, BlockMap | BlockSeq))
            && !props.has_newline
        {
            ctx.at(
                props.end,
                "MISSING_CHAR",
                "Block collection cannot start on same line with directives-end marker",
            );
        }
    }
    let contents = if let Some(value) = &mut value {
        compose_owned_node(&mut ctx, std::sync::Arc::make_mut(value), &props)
    } else {
        compose_empty_node(&mut ctx, props.end, &token.start, &props)
    };
    let content_end = ctx.nodes[contents].range[2];
    let re = resolve_end(&mut ctx, end, content_end, false);
    Document {
        contents: Some(contents),
        nodes: ctx.nodes,
        directives: ctx.directives,
        options: ctx.options,
        errors: ctx.errors,
        warnings: ctx.warnings,
        range: [token.offset, content_end, re.offset],
        comment: re.comment,
        comment_before: Default::default(),
        line_counter: Default::default(),
        schema: ctx.schema,
    }
}
