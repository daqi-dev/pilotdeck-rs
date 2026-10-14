// Port of yaml@2.9.0 dist/schema/yaml-1.1/omap.js (ISC), resolve.
use crate::{
    compose::Context,
    nodes::{NodeId, NodeKind, ScalarValue},
    parse::cst::Token,
    string::message,
};
pub(crate) fn resolve(ctx: &mut Context, id: NodeId, tag: &Token) {
    super::pairs::resolve(ctx, id, tag);
    let NodeKind::Pairs(pairs) = ctx.nodes[id].kind.clone() else {
        return;
    };
    let mut seen: Vec<ScalarValue> = Vec::new();
    for pair in &pairs {
        if let NodeKind::Scalar(value) = &ctx.nodes[pair.key].kind {
            // JS includes compares objects and Symbols by identity. Distinct
            // parsed scalar nodes hold distinct Date, Buffer and Symbol values.
            if matches!(
                value,
                ScalarValue::Binary(_) | ScalarValue::Timestamp(_) | ScalarValue::Merge
            ) {
                continue;
            }
            let value = value.clone();
            let same = seen.iter().any(|a| {
                a == &value
                    || matches!((a, &value),
                (ScalarValue::Number(a), ScalarValue::Number(b)) if a.is_nan() && b.is_nan())
            });
            if same {
                ctx.token_error(
                    tag,
                    "TAG_RESOLVE_FAILED",
                    message(
                        "Ordered maps must not include duplicate keys: ",
                        &crate::nodes::to_js::scalar_string(&value),
                    ),
                );
            } else {
                seen.push(value);
            }
        }
    }
    ctx.nodes[id].kind = NodeKind::OMap(pairs);
}
