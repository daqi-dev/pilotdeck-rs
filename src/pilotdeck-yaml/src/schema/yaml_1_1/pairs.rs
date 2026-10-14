// Port of yaml@2.9.0 dist/schema/yaml-1.1/pairs.js (ISC), resolvePairs.
use crate::{
    compose::Context,
    nodes::{Node, NodeId, NodeKind, Pair, ScalarValue},
    parse::cst::Token,
    string::concat,
};
pub(crate) fn resolve(ctx: &mut Context, id: NodeId, tag: &Token) {
    let NodeKind::Seq(items) = ctx.nodes[id].kind.clone() else {
        ctx.token_error(
            tag,
            "TAG_RESOLVE_FAILED",
            "Expected a sequence for this tag",
        );
        return;
    };
    let mut pairs = Vec::new();
    for item in items {
        let mut node = ctx.nodes[item].clone();
        let pair = if let NodeKind::Map(items) = &node.kind {
            if items.len() > 1 {
                ctx.token_error(
                    tag,
                    "TAG_RESOLVE_FAILED",
                    "Each pair must have its own sequence indicator",
                );
            }
            let pair = items.first().cloned().unwrap_or_else(|| Pair {
                key: ctx.add(Node::new(NodeKind::Scalar(ScalarValue::Null))),
                value: None,
            });
            if !node.comment_before.is_empty() {
                let key = &mut ctx.nodes[pair.key];
                key.comment_before = if key.comment_before.is_empty() {
                    std::mem::take(&mut node.comment_before)
                } else {
                    concat(&[&node.comment_before, &"\n".into(), &key.comment_before])
                };
            }
            if !node.comment.is_empty() {
                let cn = &mut ctx.nodes[pair.value.unwrap_or(pair.key)];
                cn.comment = if cn.comment.is_empty() {
                    std::mem::take(&mut node.comment)
                } else {
                    concat(&[&node.comment, &"\n".into(), &cn.comment])
                };
            }
            pair
        } else {
            Pair {
                key: item,
                value: None,
            }
        };
        pairs.push(pair);
    }
    ctx.nodes[id].kind = NodeKind::Pairs(pairs);
}
