// Port of yaml@2.9.0 dist/schema/yaml-1.1/set.js (ISC), resolve.
use crate::{
    compose::Context,
    nodes::{NodeId, NodeKind, ScalarValue},
    parse::cst::Token,
};
pub(crate) fn resolve(ctx: &mut Context, id: NodeId, tag: &Token) {
    let NodeKind::Map(pairs) = ctx.nodes[id].kind.clone() else {
        ctx.token_error(tag, "TAG_RESOLVE_FAILED", "Expected a mapping for this tag");
        return;
    };
    // npm composes this tag with YAMLSet as its nodeClass. A resolve error
    // reports invalid values but leaves that Set node intact for toJSON().
    let valid = pairs.iter().all(|p| {
        p.value.is_none_or(|v| {
            let n = &ctx.nodes[v];
            matches!(n.kind, NodeKind::Scalar(ScalarValue::Null))
                && n.comment.is_empty()
                && n.comment_before.is_empty()
                && n.tag.is_none()
        })
    });
    ctx.nodes[id].kind = NodeKind::Set(pairs);
    if !valid {
        ctx.token_error(
            tag,
            "TAG_RESOLVE_FAILED",
            "Set items must all have null values",
        );
    }
}
