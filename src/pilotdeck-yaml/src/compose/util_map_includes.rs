// Port of yaml@2.9.0 dist/compose/util-map-includes.js (ISC).
use super::Context;
use crate::nodes::{NodeId, NodeKind, Pair, ScalarValue};
pub(super) fn map_includes(ctx: &Context, items: &[Pair], search: NodeId) -> bool {
    items.iter().any(|p| {
        p.key == search
            || match (&ctx.nodes[p.key].kind, &ctx.nodes[search].kind) {
                (NodeKind::Scalar(a), NodeKind::Scalar(b)) => match (a, b) {
                    (ScalarValue::Merge, _) | (_, ScalarValue::Merge) => false,
                    (ScalarValue::Timestamp(_) | ScalarValue::Binary(_), _) => false,
                    _ => a == b,
                },
                _ => false,
            }
    })
}
