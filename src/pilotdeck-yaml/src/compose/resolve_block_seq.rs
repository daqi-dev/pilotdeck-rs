// Port of yaml@2.9.0 dist/compose/resolve-block-seq.js (ISC).
use super::{
    Context,
    compose_node::{compose_empty_node, compose_node},
    resolve_props::{PropOptions, resolve_props},
};
use crate::{
    nodes::{Node, NodeId, NodeKind},
    parse::cst::{Token, TokenType::*},
};
pub(super) fn resolve_block_seq(ctx: &mut Context, bs: &Token) -> NodeId {
    let mut seq = Node::new(NodeKind::Seq(Vec::new()));
    let mut items = Vec::new();
    ctx.at_root = false;
    ctx.at_key = false;
    let mut offset = bs.offset;
    let mut comment_end = None;
    for it in &bs.items {
        let value = it.value.as_deref();
        let props = resolve_props(
            ctx,
            &it.start,
            PropOptions {
                flow: None,
                indicator: SeqItemInd,
                next: value,
                offset,
                parent_indent: bs.indent.unwrap(),
                start_on_newline: true,
            },
        );
        if props.found.is_none() {
            if props.anchor.is_some() || props.tag.is_some() || value.is_some() {
                if value.is_some_and(|v| v.kind == BlockSeq) {
                    ctx.at(
                        props.end,
                        "BAD_INDENT",
                        "All sequence items must start at the same column",
                    );
                } else {
                    ctx.at(offset, "MISSING_CHAR", "Sequence item without - indicator");
                }
            } else {
                comment_end = Some(props.end);
                if !props.comment.is_empty() {
                    seq.comment = props.comment;
                }
                continue;
            }
        }
        let node = if let Some(value) = value {
            compose_node(ctx, value, &props)
        } else {
            compose_empty_node(ctx, props.end, &it.start, &props)
        };
        offset = ctx.nodes[node].range[2];
        items.push(node);
    }
    seq.kind = NodeKind::Seq(items);
    seq.range = [bs.offset, offset, comment_end.unwrap_or(offset)];
    ctx.add(seq)
}
